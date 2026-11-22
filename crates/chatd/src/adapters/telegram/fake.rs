//! A wire with a script behind it and no network: a small invented account that answers the
//! requests the adapter makes and sends updates when a test says so. It exists so the adapter
//! can be driven through chatd's real socket (`tests/telegram.rs`); it is reachable only in a
//! scratch directory with `TINYCLIENT_TELEGRAM_FAKE` set, never from a real session.
//!
//! scratch directory (`adapters/script.rs`): `telegram.signin.ctl` while signing in (`scan`,
//! `scan-other`, `scan-password`), `telegram.ctl` once signed in (`incoming`, `photo`, `react`, `edit`,
//! `delete`, `ttl`, `ttl-gone`, `topic`, `comments`, `typing`, `typing-stop`, `read`, `status`,
//! `poll`, `poll-full`, `poll-update`, `poll-counts`, `voice`, `link`, `cdn-file`, `big-file`,
//! `forward`, `flood-send N`, `flood-file N`, `photo-pages N`, `revoke`, `photo-preview`,
//! `secret`, `secret-gone`, and the holds of `TINYCLIENT_TELEGRAM_HOLD`,
//! `TINYCLIENT_TELEGRAM_PEER_HOLD` and `TINYCLIENT_TELEGRAM_FILE_HOLD` through `dialogs-go`,
//! `peer-go` and `file-go`). Every request it is sent is appended to `telegram.calls`, one line
//! each, so a test can see what the network would have heard.
//!
//! The account: Mira Kessler (7001); Odalys Brandt (7002, a contact, with a second password on
//! her own number); Tobin Reyes (7003); a group "Trail crew" (8001); a channel "Field Notes"
//! (9001, muted, with a sponsored message) with a linked discussion group "Field Notes chat"
//! (9003, which the comments on its post come from, heard of at `getDiscussionMessage`); a
//! forum "Workshop" (9002) with topics General, "Tools" (31) and "Jigs" (55, the newest
//! unread). Workshop's member list is its owner, two admins and its bot. A search there for
//! Nia Pell (7012) names her; she is not otherwise in the account. The phone +1 555 010 0142
//! signs in with the code 24680; +1 555 010 0199 also wants the password "correct horse".
//! Username resolution serves `gif`, `forest_wren`, and the public chats a search can preview
//! (`larchpress`, `larchtrail`, `closedridge`, `fullshelf`, `similarpeaks`). `flood_wait` returns
//! a flood wait, and every other name returns USERNAME_NOT_OCCUPIED.

use super::peers::Who;
use super::wire::{Event, Fault, Wire};
use crate::adapters::script;
use grammers_crypto::two_factor_auth::calculate_2fa;
use grammers_tl_types as tl;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, PoisonError, Mutex};
use tl::{Deserializable, Identifiable, RemoteCall, Serializable};

const ME: i64 = 7001;
const ODALYS: i64 = 7002;
const TOBIN: i64 = 7003;
const CREW: i64 = 8001;
const NOTES: i64 = 9001;
const WORKSHOP: i64 = 9002;
/// "Field Notes chat", the channel's linked discussion group: where its post's comments live.
const CHATTERS: i64 = 9003;
const LEFT_BROADCAST: i64 = 9004;
const INVALID_CHANNEL: i64 = 9005;
const OFF_PAGE_GROUP: i64 = 9006;
const OFF_PAGE_HANDLE: &str = "weatherdesk";
/// Public chats a search can preview. None of them is in the dialog list until joined.
const LARCH: i64 = 9010;
const CLOSED: i64 = 9011;
const FULL: i64 = 9012;
const SIMILAR: i64 = 9013;
const LARCH_TRAIL: i64 = 9014;
/// The post's echo in the discussion group: the message a send answers, and the id the echo
/// test looks for (`GetDiscussionMessage` names it for the open, the send for its reply).
const ECHO: i32 = 60;
const NEWEST_COMMENT: i32 = ECHO + 2;
const TOOLS: i32 = 31;
/// The forum's newest topic, active after "Tools": the one opening the forum lands on.
const JIGS: i32 = 55;
/// Telegram's own account, a bot, an account Telegram flags as a scam, and one it flags an impostor.
const SERVICE: i64 = 777_000;
const REPLIES: i64 = 1_271_266_957;
const BOT: i64 = 7004;
/// Named only by a member search of Workshop. Not in `users()`, so a contact search never finds her.
const NIA: i64 = 7012;
const SCAM: i64 = 7005;
const FAKE: i64 = 7006;
/// The `@gif` bot.
const GIF_BOT: i64 = 7050;
/// An inline bot named `@wiki`, for a query that is not a GIF search.
const WIKI_BOT: i64 = 7060;
/// The folders: Work (the group and the forum) and Family (Odalys pinned, and the group too).
const WORK: i32 = 2;
const FAMILY: i32 = 3;
/// Extra people, each with a chat of their own, when `TINYCLIENT_TELEGRAM_MANY` asks for them.
const CROWD: i64 = 100_000;
const CODE: &str = "24680";
const PASSWORD: &str = "correct horse";
/// 2026-10-13 12:00 UTC, so every run stores the same times unless the harness says otherwise.
const FIXED_T0: i32 = 1_791_892_800;
/// Where the fake's clock starts, in unix seconds. `TINYCLIENT_TELEGRAM_T0` moves it: the Inbox leaves out
/// a chat nobody touched for thirty days, so a world pinned to one date stops matching the day it ages out.
fn t0() -> i32 {
    static T0: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *T0.get_or_init(|| std::env::var("TINYCLIENT_TELEGRAM_T0").ok().and_then(|seconds| seconds.parse().ok()).unwrap_or(FIXED_T0))
}
/// The prime Telegram itself uses for the password proof, with generator 3.
const PRIME: &str = "c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f48198a0aa7c14058229493d22530f4dbfa336f6e0ac925139543aed44cce7c3720fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18faf8c595f642477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4a4a695811051907e162753b56b0f6b410dba74d8a84b2a14b3144e0ef1284754fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9ae4e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f0d8115f635b105ee2e4e15d04b2454bf6f4fadf034b10403119cd8e3b92fcc5b";

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap_or(0)).collect()
}

/// `m***@example.com`. The local part's first letter only; the rest stays hidden.
fn mask_email(email: &str) -> String {
    let Some((name, host)) = email.split_once('@') else {
        return "email".to_owned();
    };
    let first = name.chars().next().unwrap_or('*');
    format!("{first}***@{host}")
}

/// One field of a value, written out.
type Field<'a> = &'a dyn Fn(&mut Vec<u8>);

/// A value read back from the fewest bytes that make one: every flag off, every optional field
/// absent. Telegram's types have dozens of fields; a script sets the few it means.
fn blank<T: Deserializable>(fields: &[Field]) -> T {
    let mut bytes = Vec::new();
    fields.iter().for_each(|field| field(&mut bytes));
    T::from_bytes(&bytes).expect("a blank value reads back")
}

fn put<T: Serializable>(value: T) -> impl Fn(&mut Vec<u8>) {
    move |bytes| value.serialize(bytes)
}

fn user(id: i64, first: &str, last: &str, username: &str) -> tl::types::User {
    let mut user: tl::types::User = blank(&[&put(0u32), &put(0u32), &put(id)]);
    user.access_hash = Some(id * 1_000 + 7);
    user.first_name = Some(first.to_owned());
    user.last_name = (!last.is_empty()).then(|| last.to_owned());
    user.username = (!username.is_empty()).then(|| username.to_owned());
    user.is_self = id == ME;
    user.contact = id == ODALYS;
    if id == ODALYS {
        user.status = Some(tl::types::UserStatusOnline { expires: t0() + 300 }.into());
        user.photo = Some(tl::types::UserProfilePhoto { has_video: false, personal: false, photo_id: 5_002, stripped_thumb: None, dc_id: 2 }.into());
    }
    if id == TOBIN {
        user.status = Some(tl::enums::UserStatus::Recently(blank(&[&put(0u32)])));
    }
    let peer = std::env::var("TINYCLIENT_TELEGRAM_PEER_PREMIUM").ok();
    let id_text = id.to_string();
    user.premium = (id == ME && std::env::var("TINYCLIENT_TELEGRAM_PREMIUM").ok().as_deref() == Some("1")) || peer.as_deref() == Some(id_text.as_str());
    user.verified = id == SERVICE;
    user.bot = id == BOT || id == GIF_BOT || id == WIKI_BOT;
    // A bot's flag is shared with its info version, which must then be there.
    user.bot_info_version = user.bot.then_some(1);
    user.scam = id == SCAM;
    user.fake = id == FAKE;
    user.phone = (id == ODALYS).then(|| "15550100777".to_owned());
    if id >= CROWD {
        user.photo = Some(tl::types::UserProfilePhoto { has_video: false, personal: false, photo_id: id + 5_000_000, stripped_thumb: None, dc_id: 2 }.into());
    }
    user
}

/// How many extra chats the account has, for the tests of a long chat list.
fn crowd() -> i64 {
    std::env::var("TINYCLIENT_TELEGRAM_MANY").ok().and_then(|many| many.parse().ok()).unwrap_or(0)
}

/// The tests say which asks may be held at the wire until they steer them through.
fn held_at_wire(name: &str) -> bool {
    std::env::var(name).is_ok_and(|held| held == "1")
}

fn users() -> Vec<tl::enums::User> {
    let mut all: Vec<tl::enums::User> = vec![
        user(ME, "Mira", "Kessler", "mira_k").into(),
        user(ODALYS, "Odalys", "Brandt", "odalys").into(),
        user(TOBIN, "Tobin", "Reyes", "").into(),
        user(SERVICE, "Telegram", "", "").into(),
        user(BOT, "Trail Bot", "", "trail_bot").into(),
        user(SCAM, "Prize Desk", "", "").into(),
        user(FAKE, "Support Desk", "", "").into(),
    ];
    all.extend((0..crowd()).map(|at| user(CROWD + at, "Person", &at.to_string(), "").into()));
    all
}

fn a_channel(id: i64, title: &str, broadcast: bool, forum: bool, username: &str) -> tl::types::Channel {
    let mut channel: tl::types::Channel =
        blank(&[&put(0u32), &put(0u32), &put(id), &put(title.to_owned()), &put(tl::enums::ChatPhoto::Empty), &put(t0() - 86_400)]);
    channel.access_hash = Some(id * 1_000 + 9);
    channel.broadcast = broadcast;
    channel.megagroup = !broadcast;
    channel.forum = forum;
    channel.username = (!username.is_empty()).then(|| username.to_owned());
    channel
}

/// A public channel or megagroup a search can name. `joinable` is false when joining is refused.
struct PublicChat {
    id: i64,
    title: &'static str,
    broadcast: bool,
    username: &'static str,
    joinable: bool,
}

fn public_catalog() -> &'static [PublicChat] {
    &[
        PublicChat { id: LARCH, title: "Larch Press", broadcast: true, username: "larchpress", joinable: true },
        PublicChat { id: LARCH_TRAIL, title: "Larch Trail", broadcast: false, username: "larchtrail", joinable: true },
        PublicChat { id: CLOSED, title: "Closed Ridge", broadcast: false, username: "closedridge", joinable: false },
        PublicChat { id: FULL, title: "Full Shelf", broadcast: true, username: "fullshelf", joinable: false },
        PublicChat { id: SIMILAR, title: "Similar Peaks", broadcast: true, username: "similarpeaks", joinable: true },
    ]
}

/// Not a member yet: `left` stays set until `channels.joinChannel` answers.
fn public_chat(spec: &PublicChat) -> tl::enums::Chat {
    let mut channel = a_channel(spec.id, spec.title, spec.broadcast, false, spec.username);
    channel.left = true;
    tl::enums::Chat::Channel(channel)
}

/// Channels and groups this query names. Wren stays in every search answer on her own.
#[inline(never)]
fn searched_public(query: &str) -> Vec<tl::enums::Chat> {
    let query = query.to_ascii_lowercase();
    public_catalog()
        .iter()
        .filter(|spec| {
            (query.contains("larch") && (spec.id == LARCH || spec.id == LARCH_TRAIL))
                || (query.contains("closed") && spec.id == CLOSED)
                || (query.contains("shelf") && spec.id == FULL)
        })
        .map(public_chat)
        .collect()
}

fn chats() -> Vec<tl::enums::Chat> {
    let crew = tl::types::Chat {
        creator: false,
        left: false,
        deactivated: false,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id: CREW,
        title: "Trail crew".to_owned(),
        photo: tl::enums::ChatPhoto::Empty,
        participants_count: 3,
        date: t0() - 86_400,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    };
    vec![
        crew.into(),
        tl::enums::Chat::Channel({
            let mut notes = a_channel(NOTES, "Field Notes", true, false, "fieldnotes");
            notes.verified = true;
            notes
        }),
        tl::enums::Chat::Channel({
            let mut workshop = a_channel(WORKSHOP, "Workshop", false, true, "");
            if std::env::var("TINYCLIENT_TELEGRAM_ADMIN").ok().as_deref() == Some("1") {
                let mut rights: tl::types::ChatAdminRights = blank(&[&put(0u32)]);
                rights.delete_messages = true;
                rights.pin_messages = true;
                rights.manage_topics = true;
                workshop.admin_rights = Some(rights.into());
            }
            workshop
        }),
    ]
}

fn discussion_group(left: bool) -> tl::types::Channel {
    let mut group = a_channel(CHATTERS, "Field Notes chat", false, false, "");
    group.left = left;
    group.has_link = true;
    group
}

fn off_page_group() -> tl::types::Channel {
    a_channel(OFF_PAGE_GROUP, "Weather desk", false, false, OFF_PAGE_HANDLE)
}

fn poll_words(text: &str) -> tl::enums::TextWithEntities {
    tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities { text: text.to_owned(), entities: Vec::new() })
}

/// Saturday or Sunday, with a count on each option. The first option is not the one we chose.
fn crew_poll(counts: &[(u8, i32)], total: i32) -> (tl::enums::Poll, tl::enums::PollResults) {
    let answers = [("Saturday", 0u8), ("Sunday", 1u8)];
    let poll = tl::types::Poll {
        id: 44,
        closed: false,
        public_voters: false,
        multiple_choice: false,
        quiz: false,
        open_answers: false,
        revoting_disabled: false,
        shuffle_answers: false,
        hide_results_until_close: false,
        creator: false,
        subscribers_only: false,
        question: poll_words("Saturday or Sunday?"),
        answers: answers
            .iter()
            .map(|(text, option)| {
                tl::types::PollAnswer {
                    text: poll_words(text),
                    option: vec![*option],
                    media: None,
                    added_by: None,
                    date: None,
                }
                .into()
            })
            .collect(),
        close_period: None,
        close_date: None,
        countries_iso2: None,
        hash: 0,
    };
    let results = tl::types::PollResults {
        min: false,
        has_unread_votes: false,
        can_view_stats: false,
        results: Some(
            counts
                .iter()
                .map(|(option, count)| {
                    tl::types::PollAnswerVoters {
                        chosen: false,
                        correct: false,
                        option: vec![*option],
                        voters: Some(*count),
                        recent_voters: None,
                    }
                    .into()
                })
                .collect(),
        ),
        total_voters: Some(total),
        recent_voters: None,
        solution: None,
        solution_entities: None,
        solution_media: None,
    };
    (poll.into(), results.into())
}

fn trail_keyboard() -> tl::enums::ReplyMarkup {
    tl::types::ReplyKeyboardMarkup {
        resize: true,
        single_use: false,
        selective: false,
        persistent: false,
        rows: vec![tl::types::KeyboardButtonRow {
            buttons: vec![tl::types::KeyboardButton { style: None, text: "Ridge".to_owned() }.into()],
        }
        .into()],
        placeholder: Some("Ask the trail".to_owned()),
    }
    .into()
}

fn trail_bot_info() -> tl::enums::BotInfo {
    let command = |name: &str, description: &str| {
        tl::enums::BotCommand::Command(tl::types::BotCommand { command: name.to_owned(), description: description.to_owned() })
    };
    tl::enums::BotInfo::Info(tl::types::BotInfo {
        has_preview_medias: false,
        user_id: Some(BOT),
        description: Some("Trail reports.".to_owned()),
        description_photo: None,
        description_document: None,
        commands: Some(vec![
            command("start", "Open the trail"),
            command("report", "Today's report"),
            command("closures", "Weekend closures"),
        ]),
        menu_button: Some(tl::enums::BotMenuButton::Button(tl::types::BotMenuButton {
            text: "Map".to_owned(),
            url: "https://t.me/trail_bot".to_owned(),
        })),
        privacy_policy_url: None,
        app_settings: None,
        verifier_settings: None,
    })
}

fn message(peer: Who, id: i32, from: i64, date: i32, text: &str) -> tl::types::Message {
    let mut message: tl::types::Message = blank(&[&put(0u32), &put(0u32), &put(id), &put(peer.peer()), &put(date), &put(text.to_owned())]);
    message.out = from == ME;
    // A channel's post has no author; everything else names one.
    message.from_id = (peer != Who::Channel(NOTES)).then(|| Who::User(from).peer());
    message.post = peer == Who::Channel(NOTES);
    message
}

fn in_topic(mut message: tl::types::Message, topic: i32) -> tl::types::Message {
    let mut header: tl::types::MessageReplyHeader = blank(&[&put(0u32)]);
    header.forum_topic = true;
    header.reply_to_msg_id = Some(topic);
    message.reply_to = Some(header.into());
    message
}

/// One plain and one custom reaction, the way a comment's carry them.
fn comment_reactions() -> tl::types::MessageReactions {
    let count = |reaction: tl::enums::Reaction, count, mine: bool| {
        tl::enums::ReactionCount::Count(tl::types::ReactionCount { chosen_order: mine.then_some(0), reaction, count })
    };
    tl::types::MessageReactions {
        min: false,
        can_see_list: true,
        reactions_as_tags: false,
        results: vec![
            count(tl::enums::Reaction::Emoji(tl::types::ReactionEmoji { emoticon: "👍".to_owned() }), 2, false),
            count(tl::enums::Reaction::CustomEmoji(tl::types::ReactionCustomEmoji { document_id: 5002 }), 1, false),
        ],
        recent_reactions: None,
        top_reactors: None,
    }
}

fn settings(mute_until: Option<i32>) -> tl::enums::PeerNotifySettings {
    let mut settings: tl::types::PeerNotifySettings = blank(&[&put(0u32)]);
    settings.mute_until = mute_until;
    settings.into()
}

fn dialog(peer: Who, top: i32, read: i32, outbox: i32, unread: i32, pinned: bool, folder: Option<i32>) -> tl::enums::Dialog {
    tl::types::Dialog {
        pinned,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: peer.peer(),
        top_message: top,
        read_inbox_max_id: read,
        read_outbox_max_id: outbox,
        unread_count: unread,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        notify_settings: settings((peer == Who::Channel(NOTES)).then_some(i32::MAX)),
        pts: None,
        draft: None,
        folder_id: folder,
        ttl_period: None,
    }
    .into()
}

/// Telegram's word for the text a chat's draft holds, written by its own date.
fn draft_message(text: &str) -> tl::enums::DraftMessage {
    draft_with(text, None)
}

fn draft_with(text: &str, entities: Option<Vec<tl::enums::MessageEntity>>) -> tl::enums::DraftMessage {
    let mut draft: tl::types::DraftMessage = blank(&[&put(0u32), &put(text.to_owned()), &put(t0())]);
    draft.entities = entities;
    draft.into()
}

/// Puts the chat's cloud draft on its dialog, as every dialog Telegram sends carries it.
/// The map says whose chat it is; a forum's own row carries none, its topics hold theirs.
fn drafted(world: &World, dialog: &mut tl::enums::Dialog, who: Who) {
    let Some(text) = world.drafts.get(&(who, None)) else {
        return;
    };
    let tl::enums::Dialog::Dialog(plain) = dialog else {
        return;
    };
    plain.draft = Some(draft_message(text));
}

/// One photo of an album. The caption lives on the first item; the rest share its grouped id.
fn album_photo(peer: Who, id: i32, from: i64, date: i32, text: &str, photo_id: i64, group: i64) -> tl::types::Message {
    let mut message = message(peer, id, from, date, text);
    message.media = Some(tl::types::MessageMediaPhoto {
        spoiler: false,
        live_photo: false,
        photo: Some(photo(photo_id)),
        ttl_seconds: None,
        video: None,
    }.into());
    message.grouped_id = Some(group);
    message
}

fn photo(id: i64) -> tl::enums::Photo {
    tl::types::Photo {
        has_stickers: false,
        id,
        access_hash: id + 1,
        file_reference: vec![1, 2, 3],
        date: t0(),
        sizes: vec![
            tl::types::PhotoStrippedSize { r#type: "i".to_owned(), bytes: vec![1, 20, 30] }.into(),
            tl::types::PhotoSize { r#type: "m".to_owned(), w: 320, h: 240, size: 4_096 + 9 }.into(),
            tl::types::PhotoSize { r#type: "x".to_owned(), w: 800, h: 600, size: 90_000 }.into(),
            tl::types::PhotoSize { r#type: "y".to_owned(), w: 1_280, h: 960, size: 200_000 }.into(),
        ],
        video_sizes: None,
        dc_id: 4,
    }
    .into()
}

/// The bytes the fake serves for any file: not a picture, but the same every time, and longer
/// than one piece so the adapter has to ask twice.
pub const FILE_BYTES: usize = 256 * 1024 + 9;

/// The posts `photo-pages` writes into the channel are photos from this id up, each served as
/// [`FILE_BYTES`] like any other file.
const PAGE_PHOTOS: i64 = 7_100;
/// The photo `photo-pages` writes into the group. Its row's size is a real picture, a field of
/// magenta across a green band, so a test can see it land on screen.
const CREW_PHOTO: i64 = 6_020;
const CREW_PHOTO_M: &[u8] = include_bytes!("fake_crew_m.jpg");

fn crew_photo() -> tl::enums::Photo {
    let tl::enums::Photo::Photo(mut kept) = photo(CREW_PHOTO) else { unreachable!() };
    kept.sizes[1] = tl::types::PhotoSize { r#type: "m".to_owned(), w: 320, h: 240, size: CREW_PHOTO_M.len() as i32 }.into();
    kept.into()
}

fn is_crew_thumb(location: &tl::enums::InputFileLocation) -> bool {
    matches!(location, tl::enums::InputFileLocation::InputPhotoFileLocation(photo) if photo.id == CREW_PHOTO && photo.thumb_size == "m")
}


/// A photo whose row's size is a real picture, sent with the stripped thumbnail Telegram puts
/// inside a message: what `photo-preview` posts. Orange beside blue, under fine dark stripes the
/// stripped thumbnail is too small to keep, so a test can tell the two apart on screen.
const RIDGE: i64 = 6_016;
const RIDGE_M: &[u8] = include_bytes!("fake_ridge_m.jpg");
/// Telegram's form: version 1, height 30, width 40, then the JPEG's picture without its header.
const RIDGE_STRIPPED: &[u8] = include_bytes!("fake_ridge_i.bin");

fn ridge() -> tl::enums::Photo {
    let tl::enums::Photo::Photo(mut kept) = photo(RIDGE) else { unreachable!() };
    kept.sizes[0] = tl::types::PhotoStrippedSize { r#type: "i".to_owned(), bytes: RIDGE_STRIPPED.to_vec() }.into();
    kept.sizes[1] = tl::types::PhotoSize { r#type: "m".to_owned(), w: 320, h: 240, size: RIDGE_M.len() as i32 }.into();
    kept.into()
}

/// The asked piece of a file that is these bytes.
fn piece_of(bytes: &[u8], offset: i64, limit: i32) -> Vec<u8> {
    let from = (offset.max(0) as usize).min(bytes.len());
    let to = (from + limit.max(0) as usize).min(bytes.len());
    bytes[from..to].to_vec()
}

fn file_piece(offset: i64, limit: i32) -> Vec<u8> {
    let from = (offset.max(0) as usize).min(FILE_BYTES);
    let to = (from + limit.max(0) as usize).min(FILE_BYTES);
    (from..to).map(|at| (at % 251) as u8).collect()
}

/// A file past one megabyte piece, so a dropped transfer has a part to resume.
const BIG_FILE: usize = 1024 * 1024 + 64;
/// What `cdn-file` serves, encrypted on the way out. The home datacenter names its checksum.
const CDN_PLAIN: &[u8] = b"ridge file from the cdn";
const CDN_KEY: [u8; 32] = [3; 32];
const CDN_IV: [u8; 16] = [5; 16];

fn big_piece(offset: i64, limit: i32) -> Vec<u8> {
    let from = (offset.max(0) as usize).min(BIG_FILE);
    let to = (from + limit.max(0) as usize).min(BIG_FILE);
    (from..to).map(|at| (at % 251) as u8).collect()
}

fn cdn_digest() -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(CDN_PLAIN);
    hasher.finalize().to_vec()
}

fn cdn_hash() -> tl::enums::FileHash {
    tl::enums::FileHash::Hash(tl::types::FileHash { offset: 0, limit: CDN_PLAIN.len() as i32, hash: cdn_digest() })
}

/// The plaintext slice at `offset`, as the CDN would send it.
fn cdn_cipher(offset: i64, limit: i32) -> Vec<u8> {
    let from = (offset.max(0) as usize).min(CDN_PLAIN.len());
    let to = (from + limit.max(0) as usize).min(CDN_PLAIN.len());
    let mut bytes = CDN_PLAIN[from..to].to_vec();
    super::files::cdn_apply(&CDN_KEY, &CDN_IV, from as u64, &mut bytes).expect("the cdn key");
    bytes
}

/// A gzip member: what Telegram serves many emoji files (a topic's icon among them) as,
/// of the file that is inside it. The checks of the container's own trailer are left for
/// whoever reads it whole: what matters here is the packed body.
fn packed(member: &[u8]) -> Vec<u8> {
    let mut held = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    held.extend(miniz_oxide::deflate::compress_to_vec(member, 6));
    held.extend(0u32.to_le_bytes());
    held.extend((member.len() as u32).to_le_bytes());
    held
}

#[derive(Clone)]
struct FakeTopic {
    id: i32,
    title: String,
    date: i32,
    top_message: i32,
    unread: i32,
    icon_color: i32,
    icon_emoji_id: Option<i64>,
    closed: bool,
    pinned: bool,
    hidden: bool,
    my: bool,
}

struct World {
    signed_in: bool,
    /// Messages by conversation, oldest first by id.
    messages: BTreeMap<String, BTreeMap<i32, tl::enums::Message>>,
    next: i32,
    /// The QR code was scanned; `Some(true)` if the account wants its password as well.
    scanned: Option<bool>,
    tokens: u8,
    /// The number a code was sent to.
    phone: Option<String>,
    /// The password is owed before the session counts.
    password_owed: bool,
    /// The next send is refused with a flood wait of this many seconds, once.
    flood_send: Option<u32>,
    /// The next `upload.GetFile` is refused with a flood wait of this many seconds, once.
    flood_file: Option<u32>,
    revoked: bool,
    /// Tobin was added to the Family folder (`folder`).
    family_grew: bool,
    /// The Work folder now takes its chats by kind and leaves read ones out (`exclude-read`).
    exclude_read: bool,
    /// A read of the group that no longer counts here, as one told during a dropped link. It
    /// stays lost until that read is told again, and then it is heard.
    lost_reads: bool,
    /// Manual unread marks (`messages.markDialogUnread` and `unread-mark`), by peer.
    unread_marks: HashMap<Who, bool>,
    /// The cloud drafts Telegram keeps, by their conversation: some typed on the phone, some
    /// told by `saveDraft` from here. The key is whose, and in a forum which topic.
    drafts: HashMap<(Who, Option<i32>), String>,
    /// Comments under the channel's post that the test says are unread.
    comments_unread: i32,
    discussion_left: bool,
    off_page_muted: bool,
    off_page_flood: bool,
    off_page_burst: bool,
    off_page_private: bool,
    membership_fault_sent: bool,
    /// A post in the muted channel that names the user: the mention count rides the dialog,
    /// so it lands with the next reading of the chats.
    channel_mention: bool,
    /// Whose session this wire is. The invented account is Mira unless a saved session says.
    me: i64,
    me_name: String,
    /// The next scan signs in a second invented account, not Mira.
    as_other: bool,
    /// How far the account read each forum topic, as its last `ReadDiscussion` said.
    topic_reads: HashMap<(Who, i32), i32>,
    /// Users `contacts.block` has blocked. `getBlocked` and `userFull.blocked` read this.
    blocked: HashSet<i64>,
    /// Per-user `(phone_calls_available, phone_calls_private, video_calls_available)` for `GetFullUser`.
    call_flags: HashMap<i64, (bool, bool, bool)>,
    /// Users whose `UserFull::voice_messages_forbidden` is true and whose `SendMedia` refuses voice notes.
    voice_forbidden: HashSet<i64>,
    /// Groups/supergroups where `send_voices` is forbidden (`CHAT_SEND_VOICES_FORBIDDEN`).
    voice_banned: HashSet<Who>,
    /// Stored privacy rules by key name (`phone_call`, `status_timestamp`, …).
    privacy: HashMap<&'static str, Vec<tl::enums::PrivacyRule>>,
    /// The next `account.setPrivacy` returns `PREMIUM_ACCOUNT_REQUIRED` (`privacy-refuse`).
    privacy_refuse: bool,
    /// `account.getAutoDownloadSettings` presets: roaming, mobile, Wi-Fi.
    download: [tl::types::AutoDownloadSettings; 3],
    /// The next `account.saveAutoDownloadSettings` returns `PREMIUM_ACCOUNT_REQUIRED`.
    download_refuse: bool,
    /// `account.getAutoDownloadSettings` fails, so a save must not write a preset.
    download_unread: bool,
    /// Channel id this account's one boost slot is applied to.
    boost_peer: Option<i64>,
    boost_level: i32,
    boost_count: i32,
    /// The next `premium.applyBoost` returns `PREMIUM_ACCOUNT_REQUIRED`.
    boost_refuse: bool,
    /// The next `messages.startBot` returns `BOT_INVALID`.
    start_refuse: bool,
    calls: PathBuf,
    /// The test let the held pages of the chats through (`dialogs-go`, `peer-go`).
    dialogs_go: bool,
    peer_go: bool,
    /// The test let the held thumbnail of `photo-preview` through (`file-go`).
    file_go: bool,
    /// The `upload.GetFile` asks being answered now, and the most at once, which the
    /// tests of the rows' pictures read (`file-count`).
    file_in: usize,
    file_at_once: usize,
    /// `getCdnFile` has been told to try the file again (`cdn-file`).
    cdn_reuploaded: bool,
    /// The first piece of `big-file` past the start has already been dropped once.
    large_broke: bool,
    /// Chats marked as protected (`noforwards`) via `steer`.
    noforwards: HashSet<Who>,
    /// Saved GIF document IDs, newest first (`messages.getSavedGifs` / `messages.saveGif`).
    saved_gifs: Vec<i64>,
    /// Current Telegram Stars balance (`payments.getStarsStatus` / `messages.sendPaidReaction`).
    stars_balance: i64,
    /// Account default quick reaction (`help.getConfig` / `messages.setDefaultReaction`).
    default_reaction: String,
    /// Paid reaction count accumulated per `(peer, msg_id)` in this session.
    paid_counts: HashMap<(Who, i32), i32>,
    /// Whether `account.getPassword` reports a recovery email (`has_recovery`).
    has_recovery: bool,
    /// Login email set during `SetUpEmailRequired` flow.
    pending_login_email: Option<String>,
    /// Whether the phone code flow is testing `authorizationSignUpRequired`.
    signup_required: bool,
    /// The Camp pack (id 920) was installed with `messages.installStickerSet`.
    camp_installed: bool,
    /// The Gold pack (id 930) is installed as well as featured. Paired with `featured_premium`.
    gold_installed: bool,
    /// Installed pack ids, first shown first (`messages.reorderStickerSets`). Empty keeps Hiking, then Camp.
    sticker_order: Vec<i64>,
    /// Starred sticker document ids (`messages.faveSticker`).
    faved: Vec<i64>,
    /// `messages.getRecentStickers` returns the Hiking sticker.
    show_recent: bool,
    /// `messages.getFeaturedStickers` returns one Premium pack.
    featured_premium: bool,
    /// Per-chat auto-delete timer in seconds (`messages.setHistoryTTL`).
    ttl_periods: HashMap<Who, i32>,
    /// Account-wide default auto-delete timer for new chats (`messages.get/setDefaultHistoryTTL`).
    default_ttl: i32,
    /// Custom/mutated dialog filters once modified via `UpdateDialogFilter` / `UpdateDialogFiltersOrder`.
    custom_folders: Option<Vec<tl::enums::DialogFilter>>,
    /// Mutable forum topics per forum channel (initialized on demand for `WORKSHOP`).
    forum_topics: HashMap<Who, Vec<FakeTopic>>,
    /// Pending scheduled messages by conversation key (`messages.getScheduledHistory`).
    scheduled: BTreeMap<String, BTreeMap<i32, tl::enums::Message>>,
    /// Account-wide notification defaults: `[users_muted, groups_muted, channels_muted]`.
    notify_defaults: [bool; 3],
    /// Per-peer notification exceptions (`account.getNotifyExceptions`).
    notify_exceptions: Vec<(Who, bool)>,
    /// Active sessions (`hash`, `app`, `device`, `region`, `country`, `current`).
    sessions: Vec<(i64, &'static str, &'static str, &'static str, &'static str, bool)>,
    /// Unread mention message IDs per conversation (`messages.getUnreadMentions`).
    unread_mentions: HashMap<String, Vec<i32>>,
    /// Unread reaction message IDs per conversation (`messages.getUnreadReactions`).
    unread_reactions: HashMap<String, Vec<i32>>,
    /// Mutable own-profile fields updated by `account.updateProfile`, `updateUsername`,
    /// `updateBirthday`, `updateEmojiStatus`, and `photos.uploadProfilePhoto`/`deletePhotos`.
    me_first: String,
    me_last: String,
    me_username: String,
    me_about: String,
    me_birthday: Option<tl::enums::Birthday>,
    me_emoji_status: Option<i64>,
    me_photo_id: Option<i64>,
    /// Groups and channels created during this test (`messages.createChat` / `channels.createChannel`).
    created_chats: Vec<tl::enums::Chat>,
    next_chat_id: i64,
    /// Exported invite links created or seeded during this test.
    invites: Vec<tl::types::ChatInviteExported>,
    /// Pending join requests awaiting admin approval.
    join_requests: Vec<tl::types::ChatInviteImporter>,
    /// Per-channel slow-mode seconds for `GetFullChannel`.
    slowmode_seconds: HashMap<i64, i32>,
    /// Channels/groups with media, stickers, GIFs, and voices banned in `default_banned_rights`.
    media_banned: HashSet<i64>,
    /// Per-peer (`user_id` or `channel_id`) paid-message price in Stars (`send_paid_messages_stars`).
    paid_stars: HashMap<i64, i64>,
    /// Next `messages.transcribeAudio` response mode: `"pending"`, `"exhausted"`, or immediate.
    transcribe_mode: String,
    /// Remaining free trial transcriptions reported by `messages.transcribeAudio`.
    transcribe_trial: Option<i32>,
}

fn key(peer: Who, topic: Option<i32>) -> String {
    match topic {
        Some(topic) => format!("{}_{topic}", peer.text()),
        None => peer.text(),
    }
}

impl World {
    fn off_page_dialog(&self) -> tl::enums::Dialog {
        let who = Who::Channel(OFF_PAGE_GROUP);
        let held = self.messages.get(&who.text());
        let top = held.and_then(|messages| messages.last_key_value()).map_or(100, |(id, _)| *id);
        let unread = held.map_or(0, |messages| messages.len() as i32);
        let folder = self.off_page_muted.then_some(1);
        let tl::enums::Dialog::Dialog(mut row) = dialog(who, top, 100, 100, unread, false, folder) else { unreachable!() };
        if self.off_page_muted {
            row.notify_settings = settings(Some(i32::MAX));
        }
        row.into()
    }

    fn new(signed_in: bool) -> World {
        let mut world = World {
            signed_in,
            messages: BTreeMap::new(),
            next: 100,
            scanned: None,
            tokens: 0,
            phone: None,
            password_owed: false,
            flood_send: None,
            flood_file: None,
            revoked: false,
            family_grew: false,
            lost_reads: false,
            unread_marks: HashMap::new(),
            exclude_read: false,
            drafts: HashMap::new(),
            comments_unread: 0,
            discussion_left: true,
            off_page_muted: false,
            off_page_flood: false,
            off_page_burst: false,
            off_page_private: held_at_wire("TINYCLIENT_TELEGRAM_PRIVATE_OFF_PAGE"),
            membership_fault_sent: false,
            channel_mention: false,
            me: ME,
            me_name: String::new(),
            as_other: false,
            topic_reads: HashMap::new(),
            blocked: HashSet::new(),
            call_flags: HashMap::new(),
            voice_forbidden: HashSet::new(),
            voice_banned: HashSet::new(),
            privacy: HashMap::from([
                ("phone_call", vec![tl::enums::PrivacyRule::PrivacyValueAllowAll]),
                ("phone_p2p", vec![tl::enums::PrivacyRule::PrivacyValueAllowContacts]),
                ("voice_messages", vec![tl::enums::PrivacyRule::PrivacyValueAllowAll]),
                ("status_timestamp", vec![tl::enums::PrivacyRule::PrivacyValueAllowAll]),
                ("phone_number", vec![tl::enums::PrivacyRule::PrivacyValueAllowContacts]),
                ("profile_photo", vec![tl::enums::PrivacyRule::PrivacyValueAllowAll]),
                ("chat_invite", vec![tl::enums::PrivacyRule::PrivacyValueAllowAll]),
            ]),
            privacy_refuse: false,
            download: [
                download_preset(true, 0, 0, 0),
                download_preset(false, 102_400, 512_000, 0),
                {
                    let mut high = download_preset(false, 1_048_576, 10_485_760, 1_048_576);
                    high.video_preload_large = true;
                    high.video_upload_maxbitrate = 64;
                    high
                },
            ],
            download_refuse: false,
            download_unread: false,
            boost_peer: None,
            boost_level: 3,
            boost_count: 12,
            boost_refuse: false,
            start_refuse: false,
            calls: protocol::support_dir().join("telegram.calls"),
            dialogs_go: false,
            peer_go: false,
            file_go: false,
            file_in: 0,
            file_at_once: 0,
            cdn_reuploaded: false,
            large_broke: false,
            noforwards: HashSet::new(),
            saved_gifs: vec![6_200],
            stars_balance: 250,
            default_reaction: "❤".to_owned(),
            paid_counts: HashMap::new(),
            has_recovery: false,
            pending_login_email: None,
            signup_required: false,
            camp_installed: false,
            gold_installed: false,
            sticker_order: Vec::new(),
            faved: Vec::new(),
            show_recent: false,
            featured_premium: false,
            ttl_periods: HashMap::new(),
            default_ttl: 0,
            custom_folders: None,
            forum_topics: HashMap::new(),
            scheduled: BTreeMap::new(),
            notify_defaults: [false, false, false],
            notify_exceptions: vec![(Who::Channel(NOTES), true)],
            sessions: vec![
                (0, "TinyClient", "Desktop", "Seattle", "US", true),
                (91_001, "Telegram iOS", "iPhone 15", "Seattle", "US", false),
                (91_002, "Telegram Desktop", "MacBook Pro", "Portland", "US", false),
            ],
            unread_mentions: HashMap::new(),
            unread_reactions: HashMap::new(),
            me_first: "Mira".to_owned(),
            me_last: "Kessler".to_owned(),
            me_username: "mira_k".to_owned(),
            me_about: "Maps, mostly.".to_owned(),
            me_birthday: None,
            me_emoji_status: None,
            me_photo_id: Some(5_001),
            created_chats: Vec::new(),
            next_chat_id: 9_100,
            invites: vec![tl::types::ChatInviteExported {
                revoked: false,
                permanent: true,
                request_needed: false,
                link: "https://t.me/+crew_primary".to_owned(),
                admin_id: ME,
                date: t0() - 3_600,
                start_date: None,
                expire_date: None,
                usage_limit: None,
                usage: Some(3),
                requested: None,
                subscription_expired: None,
                title: Some("Primary".to_owned()),
                subscription_pricing: None,
            }],
            join_requests: vec![
                tl::types::ChatInviteImporter {
                    requested: true,
                    via_chatlist: false,
                    user_id: ODALYS,
                    date: t0() - 120,
                    about: Some("Would love to join".to_owned()),
                    approved_by: None,
                },
                tl::types::ChatInviteImporter {
                    requested: true,
                    via_chatlist: false,
                    user_id: TOBIN,
                    date: t0() - 60,
                    about: None,
                    approved_by: None,
                },
            ],
            slowmode_seconds: HashMap::new(),
            media_banned: HashSet::new(),
            paid_stars: HashMap::new(),
            transcribe_mode: String::new(),
            transcribe_trial: Some(2),
        };
        let odalys = Who::User(ODALYS);
        world.store(None, message(odalys, 10, ODALYS, t0() - 300, "Are you coming to the ridge on Saturday?"));
        world.store(None, message(odalys, 11, ME, t0() - 240, "Yes. I'll bring the stove."));
        world.store(None, message(odalys, 12, ODALYS, t0() - 60, "Good. Trailhead at seven."));
        world.store(None, message(Who::Group(CREW), 20, TOBIN, t0() - 3_600, "Maps are in the shared folder."));
        world.store(None, message(Who::Channel(NOTES), 5, 0, t0() - 7_200, "Snow line is at 1,900 m this week."));
        let mut larch = message(Who::Channel(LARCH), 8, 0, t0() - 50, "The ridge is clear.");
        larch.from_id = None;
        larch.post = true;
        world.store(None, larch);
        world.store(None, message(Who::Channel(LARCH_TRAIL), 9, TOBIN, t0() - 40, "Meet at the larch."));
        let mut tools = message(Who::Channel(WORKSHOP), 41, TOBIN, t0() - 500, "Who has the torque wrench?");
        if held_at_wire("TINYCLIENT_TELEGRAM_FORUM_TTL") {
            tools.date = crate::hub::now() as i32;
            tools.ttl_period = Some(10);
        }
        world.store(Some(TOOLS), in_topic(tools, TOOLS));
        world.store(Some(JIGS), in_topic(message(Who::Channel(WORKSHOP), JIGS, TOBIN, t0() - 200, "First jig done: the box joints came out clean."), JIGS));
        world.store(None, message(Who::User(TOBIN), 30, TOBIN, t0() - 90_000, "Thanks for the lift."));
        // My newest word to Tobin, which his dialog's read-outbox covers: the row's ✓✓ at boot.
        world.store(None, message(Who::User(TOBIN), 31, ME, t0() - 80_000, "Trailhead at seven, then."));
        world.store(None, message(Who::User(ME), 2, ME, t0() - 100_000, "Packing list: stove, map, headlamp."));
        world.store(None, message(Who::User(SERVICE), 3, SERVICE, t0() - 110_000, "Login code: 24680. Do not give this code to anyone."));
        let mut trail = message(Who::User(BOT), 4, BOT, t0() - 120_000, "Trail report: all clear.");
        trail.reply_markup = Some(trail_keyboard());
        world.store(None, trail);
        // My newest word to the bot, which its dialog's read-outbox stops below: ✓ without ✓✓.
        world.store(None, message(Who::User(BOT), 5, ME, t0() - 115_000, "Any closures this weekend?"));
        world.store(None, message(Who::User(SCAM), 6, SCAM, t0() - 130_000, "You won a prize!"));
        world.store(None, message(Who::User(FAKE), 7, FAKE, t0() - 140_000, "Your account needs verification."));
        // A draft typed on the phone before this device signed in: it rides Odalys's dialog at
        // the next reading of it, as any draft typed elsewhere does.
        world.drafts.insert((Who::User(ODALYS), None), "Finished packing. Meet at the ridge?".to_owned());
        for at in 0..crowd() {
            let who = Who::User(CROWD + at);
            world.store(None, message(who, 1, CROWD + at, t0() - 200_000 - at as i32 * 60, &format!("Hello from {at}")));
        }
        let search_bulk: usize = std::env::var("TINYCLIENT_TELEGRAM_SEARCH_BULK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        for i in 0..search_bulk {
            let id = 1_000 + i as i32;
            let sender = if i % 2 == 0 { ODALYS } else { TOBIN };
            let date = t0() - (search_bulk as i32 - i as i32) * 86_400;
            world.store(
                None,
                message(Who::Group(CREW), id, sender, date, &format!("Bulk note {i} #ridge")),
            );
        }
        world
    }

    /// The group's read state, as the last tell of it has reached this fake: a read that was not
    /// heard keeps the unread it had before.
    fn crew(&self) -> (i32, i32) {
        match self.lost_reads {
            true => (14, 7),
            false => (20, 0),
        }
    }

    /// A manual unread mark rides the dialog the next time it is read.
    fn flag_unread(&self, dialog: &mut tl::enums::Dialog) {
        self.stamp_jump_counts(dialog);
        let tl::enums::Dialog::Dialog(row) = dialog else { return };
        let Some(on) = self.unread_marks.get(&Who::of(&row.peer)) else { return };
        row.unread_mark = *on;
    }

    /// Unread mention and reaction totals, from the lists `getUnreadMentions` and
    /// `getUnreadReactions` answer. A chat with no list keeps the count the dialog already had.
    fn stamp_jump_counts(&self, dialog: &mut tl::enums::Dialog) {
        let tl::enums::Dialog::Dialog(row) = dialog else { return };
        let place = Who::of(&row.peer).text();
        if let Some(ids) = self.unread_mentions.get(&place) {
            row.unread_mentions_count = ids.len() as i32;
        }
        if let Some(ids) = self.unread_reactions.get(&place) {
            row.unread_reactions_count = ids.len() as i32;
        }
    }

    fn folder(&self, id: i32) -> tl::enums::DialogFilter {
        let none = Vec::<tl::enums::InputPeer>::new;
        let title = tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities { text: String::new(), entities: Vec::new() });
        let mut filter: tl::types::DialogFilter = blank(&[&put(0u32), &put(id), &put(title), &put(none()), &put(none()), &put(none())]);
        let named = |text: &str| tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities { text: text.to_owned(), entities: Vec::new() });
        let crew: tl::enums::InputPeer = tl::types::InputPeerChat { chat_id: CREW }.into();
        let person = |user_id| -> tl::enums::InputPeer { tl::types::InputPeerUser { user_id, access_hash: user_id * 1_000 + 7 }.into() };
        match id {
            WORK => {
                filter.title = named("Work");
                if self.exclude_read {
                    // The same folder with "exclude read chats" on: it takes groups by kind,
                    // naming none, so a read chat leaves it.
                    filter.groups = true;
                    filter.exclude_read = true;
                } else {
                    let channel: tl::enums::InputPeer =
                        tl::types::InputPeerChannel { channel_id: WORKSHOP, access_hash: 0 }.into();
                    if lost_pin() {
                        // A pin whose chat lies past the pages the test's dialogs pass carries.
                        filter.pinned_peers = vec![crew.clone()];
                        filter.include_peers = vec![channel];
                    } else {
                        filter.include_peers = vec![channel, crew];
                    }
                }
            }
            _ => {
                filter.title = named("Family");
                filter.pinned_peers = vec![person(ODALYS)];
                filter.include_peers = vec![crew];
                if self.family_grew {
                    filter.include_peers.push(person(TOBIN));
                }
            }
        }
        filter.into()
    }

    /// What a send makes: each item a message of ours with the id Telegram gives it, told on the
    /// update stream and in the answer, or scheduled for later.
    fn deliver(&mut self, events: &Sender<Event>, peer: Who, topic: Option<i32>, reply: Option<i32>, items: Vec<(i64, String, Option<tl::enums::MessageMedia>, Option<Vec<tl::enums::MessageEntity>>)>, schedule: Option<i32>) -> Result<Vec<u8>, Fault> {
        // What was sent clears the chat's draft, on this device and everywhere.
        self.drafts.remove(&(peer, topic));
        // Two or more items are one album. Telegram stamps each with the same grouped id.
        let group = (items.len() > 1).then_some(8_000_000 + i64::from(self.next + 1));
        let mut all = Vec::new();
        for (random_id, text, media, entities) in items {
            self.next += 1;
            let id = self.next;
            let mut sent = message(peer, id, ME, schedule.unwrap_or(crate::hub::now() as i32), &text);
            sent.media = media;
            sent.entities = entities;
            sent.grouped_id = group;
            if let Some(reply_to_msg_id) = reply {
                let mut header: tl::types::MessageReplyHeader = blank(&[&put(0u32)]);
                header.reply_to_msg_id = Some(reply_to_msg_id);
                sent.reply_to = Some(header.into());
            }
            if let Some(topic) = topic {
                sent = in_topic(sent, topic);
            }
            if schedule.is_some() {
                if schedule == Some(protocol::SCHEDULE_WHEN_ONLINE as i32) && peer == Who::User(TOBIN) {
                    return Err(rpc(400, "SCHEDULE_STATUS_PRIVATE", None));
                }
                self.scheduled
                    .entry(key(peer, topic))
                    .or_default()
                    .insert(id, sent.clone().into());
                all.push(tl::types::UpdateMessageId { id, random_id }.into());
                all.push(tl::types::UpdateNewScheduledMessage { message: sent.into() }.into());
                continue;
            }
            self.store(topic, sent.clone());
            // Telegram answers a send with the id it gave and the message itself, and the
            // library hands the same two to the update stream.
            let named: tl::enums::Update = tl::types::UpdateMessageId { id, random_id }.into();
            let new: tl::enums::Update = match peer {
                Who::Channel(_) => tl::types::UpdateNewChannelMessage { message: sent.into(), pts: 0, pts_count: 0 }.into(),
                _ => tl::types::UpdateNewMessage { message: sent.into(), pts: 0, pts_count: 0 }.into(),
            };
            let _ = events.send(update(named.clone()));
            let _ = events.send(update(new.clone()));
            all.push(named);
            all.push(new);
        }
        if peer == Who::Channel(CHATTERS) {
            count_raised(self, events, &mut all);
        }
        answer(updates(all))
    }

    fn store(&mut self, topic: Option<i32>, message: tl::types::Message) {
        let peer = Who::of(&message.peer_id);
        self.messages.entry(key(peer, topic)).or_default().insert(message.id, message.into());
    }

    fn log(&self, line: &str) {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&self.calls) {
            let _ = writeln!(file, "{line}");
        }
    }

    fn self_user(&self) -> tl::types::User {
        let (id, first, last, username) = if self.as_other {
            (7_101, "Lea".to_owned(), "Hart".to_owned(), String::new())
        } else if self.me != ME && self.me != 0 {
            let mut parts = self.me_name.splitn(2, ' ');
            (self.me, parts.next().unwrap_or("Account").to_owned(), parts.next().unwrap_or("").to_owned(), String::new())
        } else {
            (ME, self.me_first.clone(), self.me_last.clone(), self.me_username.clone())
        };
        let mut person = user(id, &first, &last, &username);
        person.is_self = true;
        if id == ME {
            if let Some(pid) = self.me_photo_id {
                person.photo = Some(
                    tl::types::UserProfilePhoto { has_video: false, personal: false, photo_id: pid, stripped_thumb: None, dc_id: 2 }.into(),
                );
            }
            if let Some(doc_id) = self.me_emoji_status {
                person.emoji_status = Some(tl::types::EmojiStatus { document_id: doc_id, until: None }.into());
            }
        }
        person
    }

    fn signup_required_auth(&self) -> tl::enums::auth::Authorization {
        let terms = tl::types::help::TermsOfService {
            popup: false,
            id: tl::types::DataJson { data: "{}".to_owned() }.into(),
            text: "By signing up for Telegram, you agree not to send spam or scam users.".to_owned(),
            entities: Vec::new(),
            min_age_confirm: None,
        };
        tl::types::auth::AuthorizationSignUpRequired { terms_of_service: Some(terms.into()) }.into()
    }

    fn me(&self) -> tl::enums::auth::Authorization {
        tl::types::auth::Authorization {
            setup_password_required: false,
            otherwise_relogin_days: None,
            tmp_sessions: None,
            future_auth_token: None,
            user: self.self_user().into(),
        }
        .into()
    }

    fn password(&self) -> tl::types::account::Password {
        let algo = tl::types::PasswordKdfAlgoSha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow {
            salt1: vec![7; 8],
            salt2: vec![9; 16],
            g: 3,
            p: unhex(PRIME),
        };
        tl::types::account::Password {
            has_recovery: self.has_recovery,
            has_secure_values: false,
            has_password: true,
            current_algo: Some(algo.clone().into()),
            srp_b: Some(vec![5; 256]),
            srp_id: Some(77),
            hint: Some("the old street".to_owned()),
            email_unconfirmed_pattern: None,
            new_algo: algo.into(),
            new_secure_algo: tl::enums::SecurePasswordKdfAlgo::Unknown,
            secure_random: vec![3; 256],
            pending_reset_date: None,
            login_email_pattern: None,
        }
    }

    /// One page of a conversation, as `getHistory` and `getReplies` count it: newest first,
    /// from `offset_id` back, shifted by `add_offset`.
    ///
    /// A mention or reaction still on the unread list stays out of the page. The next jump
    /// has to land on it; once it is read it comes back with the rest of the chat.
    fn page(&self, place: &str, offset_id: i32, add_offset: i32, limit: i32) -> tl::enums::messages::Messages {
        let id = |message: &tl::enums::Message| match message {
            tl::enums::Message::Message(message) => message.id,
            tl::enums::Message::Service(message) => message.id,
            tl::enums::Message::Empty(message) => message.id,
        };
        let still_unread = |message_id: i32| {
            self.unread_mentions.get(place).is_some_and(|ids| ids.contains(&message_id))
                || self.unread_reactions.get(place).is_some_and(|ids| ids.contains(&message_id))
        };
        let all: Vec<&tl::enums::Message> = self
            .messages
            .get(place)
            .map(|held| held.values().rev().filter(|message| !still_unread(id(message))).collect())
            .unwrap_or_default();
        let start = match offset_id {
            0 => 0,
            offset => all.iter().position(|message| id(message) < offset).unwrap_or(all.len()) as i32,
        };
        let from = (start + add_offset).max(0) as usize;
        let messages = all.into_iter().skip(from).take(limit.max(0) as usize).cloned().collect();
        tl::types::messages::Messages { messages, topics: Vec::new(), chats: chats(), users: users() }.into()
    }
}

/// `help.getAppConfig`, with the documented defaults. `TINYCLIENT_TELEGRAM_CAPTION` replaces
/// the free caption so a test can see the config applied.
fn app_config() -> tl::enums::help::AppConfig {
    let caption = std::env::var("TINYCLIENT_TELEGRAM_CAPTION").ok().and_then(|value| value.parse().ok()).unwrap_or(1_024.0);
    let num = |key: &str, value: f64| -> tl::enums::JsonobjectValue {
        tl::types::JsonObjectValue { key: key.to_owned(), value: tl::types::JsonNumber { value }.into() }.into()
    };
    let config = tl::types::JsonObject {
        value: vec![
            num("caption_length_limit_default", caption),
            num("caption_length_limit_premium", 4_096.0),
            num("upload_max_fileparts_default", 4_000.0),
            num("upload_max_fileparts_premium", 8_000.0),
            num("reactions_user_max_default", 1.0),
            num("reactions_user_max_premium", 3.0),
            num("reactions_uniq_max", 11.0),
        ],
    };
    tl::enums::help::AppConfig::Config(tl::types::help::AppConfig { hash: 1, config: config.into() })
}

fn rpc(code: i32, name: &str, value: Option<u32>) -> Fault {
    Fault::Rpc { code, name: name.to_owned(), value }
}

/// The first `limit` ids, or none when Telegram asked for nothing.
fn limited(ids: &[i32], limit: i32) -> Vec<i32> {
    let n = usize::try_from(limit).unwrap_or(0);
    ids.iter().copied().take(n).collect()
}

fn boost_status(level: i32, boosts: i32, mine: bool, url: &str) -> tl::types::premium::BoostsStatus {
    tl::types::premium::BoostsStatus {
        my_boost: mine,
        level,
        current_level_boosts: 10,
        boosts,
        gift_boosts: None,
        next_level_boosts: Some(20),
        premium_audience: None,
        boost_url: url.to_owned(),
        prepaid_giveaways: None,
        my_boost_slots: mine.then_some(vec![1]),
    }
}

fn one_boost(slot: i32, channel: Option<i64>) -> tl::enums::MyBoost {
    tl::types::MyBoost {
        slot,
        peer: channel.map(|id| Who::Channel(id).peer()),
        date: t0(),
        expires: t0() + 30 * 86_400,
        cooldown_until_date: None,
    }
    .into()
}

fn download_preset(disabled: bool, photo: i32, video: i64, file: i64) -> tl::types::AutoDownloadSettings {
    tl::types::AutoDownloadSettings {
        disabled,
        video_preload_large: false,
        audio_preload_next: false,
        phonecalls_less_data: false,
        stories_preload: false,
        photo_size_max: photo,
        video_size_max: video,
        file_size_max: file,
        video_upload_maxbitrate: 0,
        small_queue_active_operations_max: 0,
        large_queue_active_operations_max: 0,
    }
}

/// Wire log name for an `account.getPrivacy` / `account.setPrivacy` key.
fn privacy_key_name(key: &tl::enums::InputPrivacyKey) -> &'static str {
    match key {
        tl::enums::InputPrivacyKey::PhoneCall => "phone_call",
        tl::enums::InputPrivacyKey::PhoneP2P => "phone_p2p",
        tl::enums::InputPrivacyKey::VoiceMessages => "voice_messages",
        tl::enums::InputPrivacyKey::StatusTimestamp => "status_timestamp",
        tl::enums::InputPrivacyKey::PhoneNumber => "phone_number",
        tl::enums::InputPrivacyKey::ProfilePhoto => "profile_photo",
        tl::enums::InputPrivacyKey::ChatInvite => "chat_invite",
        _ => "other",
    }
}

/// The request, if `body` is one of these.
fn asked<R: RemoteCall + Identifiable + Deserializable>(body: &[u8]) -> Option<R> {
    let id = u32::from_le_bytes(body.get(..4)?.try_into().ok()?);
    if id != R::CONSTRUCTOR_ID {
        return None;
    }
    // A request reads back from its fields; the id in front names which request it is.
    R::from_bytes(&body[4..]).ok()
}

fn answer<T: Serializable>(value: T) -> Result<Vec<u8>, Fault> {
    Ok(value.to_bytes())
}

// Keep these large schema values off the upload thread's small request-handler stack.
#[inline(never)]
fn resolve_username(username: &str) -> Result<Vec<u8>, Fault> {
    if username == "flood_wait" {
        return Err(rpc(420, "FLOOD_WAIT", Some(60)));
    }
    if let Some(spec) = public_catalog().iter().find(|spec| spec.username.eq_ignore_ascii_case(username)) {
        let found = tl::types::contacts::ResolvedPeer {
            peer: Who::Channel(spec.id).peer(),
            chats: vec![public_chat(spec)],
            users: Vec::new(),
        };
        return answer(tl::enums::contacts::ResolvedPeer::Peer(found));
    }
    let (id, first, last, handle) = if username.eq_ignore_ascii_case("forest_wren") {
        (7_011, "Robin", "Vale", "forest_wren")
    } else if username == "gif" {
        (GIF_BOT, "GIF Search", "", "gif")
    } else if username == "wiki" {
        (WIKI_BOT, "Wiki", "", "wiki")
    } else {
        return Err(rpc(400, "USERNAME_NOT_OCCUPIED", None));
    };
    let found = tl::types::contacts::ResolvedPeer {
        peer: Who::User(id).peer(), chats: Vec::new(), users: vec![user(id, first, last, handle).into()],
    };
    answer(tl::enums::contacts::ResolvedPeer::Peer(found))
}

/// Whether the Work folder pins a chat that no dialog page carries, what Telegram looks like
/// when a pin sits past the newest 500: its folder rule speaks of a chat nothing stored has.
fn lost_pin() -> bool {
    std::env::var_os("TINYCLIENT_TELEGRAM_LOST_PIN").is_some()
}

fn input_who(peer: &tl::enums::InputPeer) -> Who {
    match peer {
        tl::enums::InputPeer::User(user) => Who::User(user.user_id),
        tl::enums::InputPeer::Chat(chat) => Who::Group(chat.chat_id),
        tl::enums::InputPeer::Channel(channel) => Who::Channel(channel.channel_id),
        _ => Who::User(ME),
    }
}

fn updates(updates: Vec<tl::enums::Update>) -> tl::enums::Updates {
    tl::types::Updates { updates, users: users(), chats: chats(), date: t0(), seq: 0 }.into()
}

impl World {
    /// The account's drafts and a save of one. First the reading and saving of the drafts,
    /// then the pickers' asks below — this method calls out for its values to build, as the
    /// request-handler stack above it is small.
    fn draft_asks(&mut self, body: &[u8], events: &Sender<Event>) -> Option<Result<Vec<u8>, Fault>> {
        use tl::functions as f;
        if asked::<f::messages::GetAllDrafts>(body).is_some() {
            self.log(&format!("GetAllDrafts {}", self.drafts.len()));
            return Some(all_drafts(self));
        }
        let request = asked::<f::messages::SaveDraft>(body)?;
        Some(saved_draft(self, request, events))
    }
}

// Keep these large schema values off the upload thread's small request-handler stack.
#[inline(never)]
fn all_drafts(world: &World) -> Result<Vec<u8>, Fault> {
    let all: Vec<tl::enums::Update> = world
        .drafts
        .iter()
        .map(|((who, topic), text)| {
            tl::types::UpdateDraftMessage { peer: who.peer(), top_msg_id: *topic, saved_peer_id: None, draft: draft_message(text) }.into()
        })
        .collect();
    answer(updates(all))
}

/// What `getDiscussionMessage` answers for a channel post: the echo of the post, spoken in
/// the discussion group as Telegram forwards it there, leading the thread — the id a send
/// answers, learned when the thread is opened. `unread` is how many of its comments the
/// account has not read, as the test steers that.
#[inline(never)]
fn discussion_answer(place: &str, unread: i32, left: bool) -> Result<Vec<u8>, Fault> {
    if place != key(Who::Channel(NOTES), Some(5)) {
        return Err(rpc(400, "MESSAGE_ID_INVALID", None));
    }
    let mut echo = message(Who::Channel(CHATTERS), ECHO, ME, t0() - 7_200, "Snow line is at 1,900 m this week.");
    // The echo is the channel's post standing in the group: no author of its own, and the
    // forward header names the post it says again.
    echo.from_id = None;
    echo.out = false;
    let mut from: tl::types::MessageFwdHeader = blank(&[&put(0u32), &put(t0() - 7_200)]);
    from.from_id = Some(Who::Channel(NOTES).peer());
    from.channel_post = Some(5);
    echo.fwd_from = Some(from.into());
    let mut comment = message(Who::Channel(CHATTERS), NEWEST_COMMENT, ODALYS, t0() - 40, "Yes, at the hut now.");
    let mut reply: tl::types::MessageReplyHeader = blank(&[&put(0u32)]);
    reply.reply_to_msg_id = Some(ECHO);
    comment.reply_to = Some(reply.into());
    let found = tl::types::messages::DiscussionMessage {
        messages: vec![comment.into(), echo.into()],
        max_id: Some(NEWEST_COMMENT),
        read_inbox_max_id: Some(0),
        read_outbox_max_id: None,
        unread_count: unread,
        chats: vec![discussion_group(left).into()],
        users: users(),
    };
    answer(tl::enums::messages::DiscussionMessage::Message(found))
}

/// The forum's topics, as Telegram's own list names them: the account has read each topic
/// up to what its last `ReadDiscussion` said, and a read past its newest post is a read
/// whole. This runs on its own always-outlined body so the dispatcher above stays under a
/// debug build's 256 KiB connection stack.
#[inline(never)]
fn topics_answer(world: &mut World, forum: Who, request: &tl::functions::messages::GetForumTopics) -> Result<Vec<u8>, Fault> {
    let held = |messages: &BTreeMap<String, BTreeMap<i32, tl::enums::Message>>, id: i32| {
        messages.get(&key(forum, Some(id))).and_then(|list| list.values().next_back().cloned())
    };
    let newest = held(&world.messages, TOOLS);
    let top_tools = match &newest {
        Some(tl::enums::Message::Message(message)) => message.id,
        _ => 41,
    };
    let extra_topics: usize = std::env::var("TINYCLIENT_TELEGRAM_TOPICS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let list = world.forum_topics.entry(forum).or_insert_with(|| {
        let mut base = vec![
            FakeTopic { id: JIGS, title: "Jigs".to_owned(), date: t0() - 200, top_message: JIGS, unread: 2, icon_color: 3, icon_emoji_id: Some(5000), closed: false, pinned: false, hidden: false, my: false },
            FakeTopic { id: TOOLS, title: "Tools".to_owned(), date: t0() - 40_000, top_message: top_tools, unread: 1, icon_color: 3, icon_emoji_id: Some(5001), closed: false, pinned: false, hidden: false, my: false },
            FakeTopic { id: 1, title: "General".to_owned(), date: t0() - 90_000, top_message: 1, unread: 0, icon_color: 3, icon_emoji_id: Some(5002), closed: false, pinned: false, hidden: false, my: false },
        ];
        if extra_topics > base.len() {
            let need = extra_topics - base.len();
            for i in 0..need {
                let id = 200 + i as i32;
                base.push(FakeTopic {
                    id,
                    title: format!("Topic #{:03}", i + 4),
                    date: t0() - 100_000 - (i as i32) * 60,
                    top_message: id,
                    unread: 0,
                    icon_color: 3,
                    icon_emoji_id: None,
                    closed: false,
                    pinned: false,
                    hidden: false,
                    my: false,
                });
            }
        }
        base
    });
    if let Some(t) = list.iter_mut().find(|t| t.id == TOOLS) {
        t.top_message = top_tools;
    }
    let query = request.q.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(str::to_lowercase);
    let mut ordered: Vec<FakeTopic> = list
        .iter()
        .filter(|t| query.as_ref().is_none_or(|q| t.title.to_lowercase().contains(q)))
        .cloned()
        .collect();
    ordered.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| b.date.cmp(&a.date)).then_with(|| b.id.cmp(&a.id)));
    let total = ordered.len() as i32;
    let start = if request.offset_topic != 0 {
        ordered
            .iter()
            .position(|t| t.id == request.offset_topic)
            .map_or(ordered.len(), |at| at + 1)
    } else {
        0
    };
    let limit = if request.limit <= 0 { 50 } else { request.limit as usize };
    let slice: Vec<FakeTopic> = ordered.into_iter().skip(start).take(limit).collect();
    let topics: Vec<tl::enums::ForumTopic> = slice
        .iter()
        .map(|ft| {
            let read = world.topic_reads.get(&(forum, ft.id)).copied().unwrap_or(0);
            let unread = if read >= ft.top_message { 0 } else { ft.unread };
            let mut topic: tl::types::ForumTopic = blank(&[
                &put(0u32),
                &put(ft.id),
                &put(ft.date),
                &put(forum.peer()),
                &put(ft.title.clone()),
                &put(ft.icon_color),
                &put(ft.top_message),
                &put(read),
                &put(0i32),
                &put(unread),
                &put(0i32),
                &put(0i32),
                &put(0i32),
                &put(Who::User(TOBIN).peer()),
                &put(settings(None)),
            ]);
            topic.icon_emoji_id = ft.icon_emoji_id;
            topic.closed = ft.closed;
            topic.pinned = ft.pinned;
            topic.hidden = ft.hidden;
            topic.my = ft.my;
            tl::enums::ForumTopic::Topic(topic)
        })
        .collect();
    let messages: Vec<tl::enums::Message> = held(&world.messages, JIGS).into_iter().chain(newest).collect();
    let found = tl::types::messages::ForumTopics {
        order_by_create_date: false,
        count: total,
        topics,
        messages,
        chats: chats(),
        users: users(),
        pts: 1,
    };
    answer(tl::enums::messages::ForumTopics::Topics(found))
}

/// A comment went in the discussion group: the count of the post rises on the edit of it
/// Telegram tells then, on the update stream and with the answer, the way the count of any
/// post's comments keeps current (`comment_thread` keeps the id that opens the thread).
fn count_raised(world: &mut World, events: &Sender<Event>, all: &mut Vec<tl::enums::Update>) {
    let Some(tl::enums::Message::Message(mut post)) =
        world.messages.get(&key(Who::Channel(NOTES), None)).and_then(|held| held.get(&5)).cloned()
    else {
        return;
    };
    if let Some(tl::enums::MessageReplies::Replies(info)) = &mut post.replies {
        info.replies += 1;
    }
    world.store(None, post.clone());
    let raised: tl::enums::Update =
        tl::types::UpdateEditChannelMessage { message: post.into(), pts: 0, pts_count: 0 }.into();
    let _ = events.send(update(raised.clone()));
    all.push(raised);
}

/// `saveDraft` keeps a draft as the phone's do, by its conversation and its topic in a forum,
/// and says the same back to the device that saved it, as for any other ask.
#[inline(never)]
fn saved_draft(world: &mut World, request: tl::functions::messages::SaveDraft, events: &Sender<Event>) -> Result<Vec<u8>, Fault> {
    let peer = input_who(&request.peer);
    let topic = match &request.reply_to {
        Some(tl::enums::InputReplyTo::Message(reply)) => Some(reply.top_msg_id.unwrap_or(reply.reply_to_msg_id)),
        _ => None,
    };
    let noted = super::content::entity_note(request.entities.as_deref().unwrap_or(&[]));
    world.log(&format!("SaveDraft {} {}{noted}", key(peer, topic), request.message));
    let draft = request.message.clone();
    world.drafts.insert((peer, topic), draft.clone());
    let echo = tl::types::UpdateDraftMessage {
        peer: peer.peer(),
        top_msg_id: topic,
        saved_peer_id: None,
        draft: draft_with(&draft, request.entities.clone()),
    };
    let _ = events.send(update(echo.into()));
    answer(true)
}

pub struct Fake {
    world: Arc<Mutex<World>>,
    events: Sender<Event>,
    heard: Mutex<Option<Receiver<Event>>>,
    closed: Arc<AtomicBool>,
    control: PathBuf,
    /// The tests say a page may be held at the wire (`TINYCLIENT_TELEGRAM_HOLD`,
    /// `TINYCLIENT_TELEGRAM_PEER_HOLD`), until a line steers it through.
    hold_pages: bool,
    hold_peer_dialogs: bool,
    /// And the row's size of `photo-preview`'s photo (`TINYCLIENT_TELEGRAM_FILE_HOLD`), until
    /// `file-go`: what a slow download looks like while the stripped preview is on screen.
    hold_files: bool,
    /// Wakes an answer held at the wire when the test steers.
    wake: Arc<Condvar>,
}

impl Fake {
    pub fn open(saved: Option<&super::Saved>) -> Fake {
        let signed_in = saved.is_some();
        let name = if signed_in { "telegram.ctl" } else { "telegram.signin.ctl" };
        let control = script::control(name);
        let (events, heard) = channel();
        let mut world = World::new(signed_in);
        if let Some(saved) = saved {
            world.me = saved.user;
            world.me_name = saved.name.clone();
        }
        let fake = Fake {
            world: Arc::new(Mutex::new(world)),
            events,
            heard: Mutex::new(Some(heard)),
            closed: Arc::default(),
            control,
            hold_pages: held_at_wire("TINYCLIENT_TELEGRAM_HOLD"),
            hold_peer_dialogs: held_at_wire("TINYCLIENT_TELEGRAM_PEER_HOLD"),
            hold_files: held_at_wire("TINYCLIENT_TELEGRAM_FILE_HOLD"),
            wake: Arc::new(Condvar::new()),
        };
        fake.listen();
        fake
    }

    /// Reads the test's lines for as long as this wire lives; dropping the wire ends it.
    fn listen(&self) {
        let (world, events, closed, control, wake) =
            (self.world.clone(), self.events.clone(), self.closed.clone(), self.control.clone(), self.wake.clone());
        let _ = std::thread::Builder::new().name("telegram-fake".into()).spawn(move || {
            script::follow(&control, |line| {
                if closed.load(Ordering::Relaxed) {
                    return false;
                }
                if let Some(line) = line {
                    let mut world = world.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    for event in world.steer(line.trim()) {
                        let _ = events.send(event);
                    }
                    drop(world);
                    wake.notify_all();
                }
                true
            });
        });
    }

    /// Sleeps on the asks whose answers the test holds (`dialogs-go`, `peer-go`, `file-go`),
    /// saying what Telegram was asked as it is asked, whatever the hold.
    fn hold_back(&self, body: &[u8]) {
        if !self.hold_pages && !self.hold_peer_dialogs && !self.hold_files {
            return;
        }
        let world = self.world.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // The guard comes back when the test let the ask through, and its own drop sits here.
        drop(self.wake.wait_while(world, |world| self.is_held(body, world)).unwrap_or_else(PoisonError::into_inner));
    }

    /// Whether this ask's answer waits for the test to let it through.
    fn is_held(&self, body: &[u8], world: &World) -> bool {
        use tl::functions as f;
        // The chats' first page, and every later one after the `dialogs-go`; a chat's dialog
        // asked again until the `peer-go`.
        (self.hold_pages && asked::<f::messages::GetDialogs>(body).is_some_and(|request| !matches!(request.offset_peer, tl::enums::InputPeer::Empty)) && !world.dialogs_go)
            || (self.hold_peer_dialogs && asked::<f::messages::GetPeerDialogs>(body).is_some() && !world.peer_go)
            || (self.hold_files && asked::<f::upload::GetFile>(body).is_some_and(|request| is_ridge_thumb(&request.location)) && !world.file_go)
    }
}

/// The row's size of `photo-preview`'s photo.
fn is_ridge_thumb(location: &tl::enums::InputFileLocation) -> bool {
    matches!(location, tl::enums::InputFileLocation::InputPhotoFileLocation(photo) if photo.id == RIDGE && photo.thumb_size == "m")
}

impl Drop for Fake {
    fn drop(&mut self) {
        // The reader sees it within 20 ms and stops.
        self.closed.store(true, Ordering::Relaxed);
    }
}

fn update(update: tl::enums::Update) -> Event {
    Event::Update { update: Box::new(update), users: users(), chats: chats() }
}

/// An update about the discussion group, with the membership description Telegram supplied.
fn discussion_update(update: tl::enums::Update, left: bool, min: bool) -> Event {
    let mut group = discussion_group(left);
    group.min = min;
    if min {
        group.access_hash = None;
    }
    let mut users = users();
    let mut replies = user(REPLIES, "Replies", "", "replies");
    replies.bot = true;
    replies.bot_info_version = Some(1);
    users.push(replies.into());
    Event::Update { update: Box::new(update), users, chats: vec![tl::enums::Chat::Channel(group)] }
}

impl World {
    /// One line from the test: what Telegram then says unasked.
    fn steer(&mut self, line: &str) -> Vec<Event> {
        let odalys = Who::User(ODALYS);
        let new = |message: tl::types::Message| update(tl::types::UpdateNewMessage { message: message.into(), pts: 0, pts_count: 0 }.into());
        let mut words = line.split_whitespace();
        match words.next().unwrap_or("") {
            "scan" => {
                self.scanned = Some(false);
                vec![update(tl::enums::Update::LoginToken)]
            }
            "scan-other" => {
                self.as_other = true;
                self.scanned = Some(false);
                vec![update(tl::enums::Update::LoginToken)]
            }
            "scan-password" => {
                self.scanned = Some(true);
                vec![update(tl::enums::Update::LoginToken)]
            }
            // The held answers go through, as the test says (`World::held`, `World::stalled`).
            "dialogs-go" => {
                self.dialogs_go = true;
                Vec::new()
            }
            "peer-go" => {
                self.peer_go = true;
                Vec::new()
            }
            "noforwards" => {
                if let Some((who, None)) = words.next().and_then(Who::parse) {
                    self.noforwards.insert(who);
                }
                Vec::new()
            }
            "paid-reaction" => {
                // Adds a paid ⭐ reaction with a top-senders leaderboard on Workshop message 41.
                let peer = Who::Channel(WORKSHOP);
                self.paid_counts.insert((peer, 41), 5);
                let reactions = tl::types::MessageReactions {
                    min: false,
                    can_see_list: true,
                    reactions_as_tags: false,
                    results: vec![tl::enums::ReactionCount::Count(tl::types::ReactionCount {
                        chosen_order: None,
                        reaction: tl::enums::Reaction::Paid,
                        count: 5,
                    })],
                    recent_reactions: None,
                    top_reactors: Some(vec![tl::enums::MessageReactor::Reactor(tl::types::MessageReactor {
                        top: true,
                        my: false,
                        anonymous: false,
                        peer_id: Some(Who::User(ODALYS).peer()),
                        count: 5,
                    })]),
                };
                vec![update(
                    tl::types::UpdateMessageReactions {
                        peer: peer.peer(),
                        msg_id: 41,
                        top_msg_id: Some(TOOLS),
                        saved_peer_id: None,
                        reactions: reactions.into(),
                    }
                    .into(),
                )]
            }
            "file-go" => {
                self.file_go = true;
                Vec::new()
            }
            // The most file asks the wire held at once, for the tests of the rows' pictures.
            "file-count" => {
                self.log(&format!("file at once {}", self.file_at_once));
                Vec::new()
            }
            // Bold on an accented word, a link on an emoji, a mention by id, code, a spoiler.
            "incoming" => {
                let text = "Café at 🦀 dock, ask Tobin: stove.fuel() secret";
                let mut message = message(odalys, 13, ODALYS, t0() + 10, text);
                message.entities = Some(vec![
                    tl::types::MessageEntityBold { offset: 0, length: 4 }.into(),
                    tl::types::MessageEntityTextUrl { offset: 8, length: 2, url: "https://example.org/dock".to_owned() }.into(),
                    tl::types::MessageEntityMentionName { offset: 21, length: 5, user_id: TOBIN }.into(),
                    tl::types::MessageEntityCode { offset: 28, length: 12 }.into(),
                    tl::types::MessageEntitySpoiler { offset: 41, length: 6 }.into(),
                ]);
                self.store(None, message.clone());
                vec![new(message)]
            }
            // A premium emoji in the text: the entity names a document, and the text under it
            // is the plain emoji Telegram chose as the alt glyph.
            "emoji-text" => {
                let text = "Cast off 🦀 at dawn";
                let mut message = message(odalys, 22, ODALYS, t0() + 90, text);
                message.entities = Some(vec![
                    tl::types::MessageEntityCustomEmoji { offset: 9, length: 2, document_id: 5002 }.into(),
                ]);
                self.store(None, message.clone());
                vec![new(message)]
            }
            // The same while its conversation is on screen, with a still emoji's document.
            "emoji-text-live" => {
                let text = "Back at the 🔥 bench";
                let mut message = message(odalys, 23, ODALYS, t0() + 100, text);
                message.entities = Some(vec![
                    tl::types::MessageEntityCustomEmoji { offset: 12, length: 2, document_id: 5001 }.into(),
                ]);
                self.store(None, message.clone());
                vec![new(message)]
            }
            "photo" => {
                let mut message = message(odalys, 14, ODALYS, t0() + 20, "The view from the ridge");
                message.media = Some(tl::types::MessageMediaPhoto { spoiler: false, live_photo: false, photo: Some(photo(6_014)), ttl_seconds: None, video: None }.into());
                self.store(None, message.clone());
                vec![new(message)]
            }
            // A photo with a stripped thumbnail inside it, whose row's size is a real picture.
            "photo-preview" => {
                let mut message = message(odalys, 24, ODALYS, t0() + 110, "The ridge at dusk");
                message.media = Some(tl::types::MessageMediaPhoto { spoiler: false, live_photo: false, photo: Some(ridge()), ttl_seconds: None, video: None }.into());
                self.store(None, message.clone());
                vec![new(message)]
            }
            // Four photos already in the chat, one grouped id, caption on the first. No update:
            // opening the chat reads them as history.
            "album" => {
                for (id, photo_id, text) in [(40, 6_040, "Ridge in four"), (41, 6_041, ""), (42, 6_042, ""), (43, 6_043, "")] {
                    self.store(None, album_photo(odalys, id, ODALYS, t0() + 200 + id, text, photo_id, 900));
                }
                Vec::new()
            }
            // The same four arriving while the chat is open.
            "album-live" => {
                let mut events = Vec::new();
                for (id, photo_id, text) in [(60, 6_060, "Ridge in four"), (61, 6_061, ""), (62, 6_062, ""), (63, 6_063, "")] {
                    let message = album_photo(odalys, id, ODALYS, t0() + 300 + id, text, photo_id, 901);
                    self.store(None, message.clone());
                    events.push(new(message));
                }
                events
            }
            // One photo of the live album is deleted. The grid stays, one picture smaller.
            "album-drop" => {
                if let Some(held) = self.messages.get_mut(&odalys.text()) {
                    held.remove(&62);
                }
                vec![update(tl::types::UpdateDeleteMessages { messages: vec![62], pts: 0, pts_count: 1 }.into())]
            }
            // A reaction on a photo that is not the caption still belongs to the grid.
            "album-react" => {
                let count = |emoticon: &str, count, mine: bool| {
                    tl::enums::ReactionCount::Count(tl::types::ReactionCount {
                        chosen_order: mine.then_some(0),
                        reaction: tl::types::ReactionEmoji { emoticon: emoticon.to_owned() }.into(),
                        count,
                    })
                };
                let reactions = tl::types::MessageReactions {
                    min: false,
                    can_see_list: true,
                    reactions_as_tags: false,
                    results: vec![count("👍", 1, false)],
                    recent_reactions: None,
                    top_reactors: None,
                };
                vec![update(tl::types::UpdateMessageReactions { peer: odalys.peer(), msg_id: 61, top_msg_id: None, saved_peer_id: None, reactions: reactions.into() }.into())]
            }
            // A full page ends on the newest photo of an album. The other three are older than
            // that page, so a reader that stops at the page boundary shows one photo.
            "album-split" => {
                for id in 102..=150 {
                    self.store(None, message(odalys, id, ODALYS, t0() + id, "a later note"));
                }
                for (id, photo_id, text) in [(98, 6_098, "Ridge in four"), (99, 6_099, ""), (100, 6_100, ""), (101, 6_101, "")] {
                    self.store(None, album_photo(odalys, id, ODALYS, t0() + id, text, photo_id, 902));
                }
                Vec::new()
            }
            "react" => {
                let count = |emoticon: &str, count, mine: bool| {
                    tl::enums::ReactionCount::Count(tl::types::ReactionCount {
                        chosen_order: mine.then_some(0),
                        reaction: tl::types::ReactionEmoji { emoticon: emoticon.to_owned() }.into(),
                        count,
                    })
                };
                let reactions = tl::types::MessageReactions {
                    min: false,
                    can_see_list: true,
                    reactions_as_tags: false,
                    results: vec![count("👍", 2, false), count("🔥", 1, false)],
                    recent_reactions: None,
                    top_reactors: None,
                };
                vec![update(tl::types::UpdateMessageReactions { peer: odalys.peer(), msg_id: 13, top_msg_id: None, saved_peer_id: None, reactions: reactions.into() }.into())]
            }
            "custom-react" => {
                let count = |reaction: tl::enums::Reaction, count, mine: bool| {
                    tl::enums::ReactionCount::Count(tl::types::ReactionCount {
                        chosen_order: mine.then_some(0),
                        reaction,
                        count,
                    })
                };
                let reactions = tl::types::MessageReactions {
                    min: false,
                    can_see_list: true,
                    reactions_as_tags: false,
                    results: vec![
                        count(tl::enums::Reaction::Emoji(tl::types::ReactionEmoji { emoticon: "👍".to_owned() }), 2, false),
                        count(tl::enums::Reaction::CustomEmoji(tl::types::ReactionCustomEmoji { document_id: 5002 }), 1, false),
                    ],
                    recent_reactions: None,
                    top_reactors: None,
                };
                vec![update(tl::types::UpdateMessageReactions { peer: odalys.peer(), msg_id: 12, top_msg_id: None, saved_peer_id: None, reactions: reactions.into() }.into())]
            }
            // A comment gains reactions, as the thread's next page carries them: the page
            // answer is all that changes, nothing is told on the update stream.
            "comment-react" => {
                let mut comment = message(Who::Channel(CHATTERS), NEWEST_COMMENT, ODALYS, t0() - 40, "Yes, at the hut now.");
                comment.reactions = Some(comment_reactions().into());
                // The thread's page is kept under the post's id, as `getReplies` counts it.
                self.messages.entry(key(Who::Channel(NOTES), Some(5))).or_default().insert(NEWEST_COMMENT, comment.into());
                Vec::new()
            }
            // The same while the thread is open: Telegram says the reaction of a comment on
            // the discussion group's stream, naming the echo the comment answers.
            "comment-react-live" => {
                vec![update(tl::types::UpdateMessageReactions {
                    peer: Who::Channel(CHATTERS).peer(),
                    msg_id: NEWEST_COMMENT,
                    top_msg_id: Some(ECHO),
                    saved_peer_id: None,
                    reactions: comment_reactions().into(),
                }.into())]
            }
            "discussion-membership" => {
                let left = words.next() != Some("joined");
                self.discussion_left = left;
                vec![discussion_update(tl::types::UpdateChannel { channel_id: CHATTERS }.into(), left, false)]
            }
            // This member's group was absent from the sign-on dialogs. The incoming message
            // names it only as a min peer, with an optional public username.
            "off-page-group" => {
                let who = Who::Channel(OFF_PAGE_GROUP);
                let id = words.next().and_then(|id| id.parse().ok()).unwrap_or(101);
                let said = message(who, id, TOBIN, crate::hub::now() as i32, "The wind has picked up.");
                for option in words {
                    match option {
                        "muted" => self.off_page_muted = true,
                        "flood" => self.off_page_flood = true,
                        "burst" => self.off_page_burst = true,
                        _ => {}
                    }
                }
                self.store(None, said.clone());
                let mut group = off_page_group();
                group.min = true;
                group.access_hash = None;
                if self.off_page_private {
                    group.username = None;
                }
                let update = tl::types::UpdateNewChannelMessage { message: said.into(), pts: 0, pts_count: 0 };
                vec![Event::Update { update: Box::new(update.into()), users: users(), chats: vec![group.into()] }]
            }
            // The other post (root 80) has activity even though our only comment was
            // under root 60. A partial chat in the update does not refresh membership.
            "discussion-comment" => {
                let root = words.next().and_then(|id| id.parse().ok()).unwrap_or(ECHO);
                let id = words.next().and_then(|id| id.parse().ok()).unwrap_or(81);
                let mut said = message(Who::Channel(CHATTERS), id, TOBIN, crate::hub::now() as i32, "A new discussion comment");
                let mut reply: tl::types::MessageReplyHeader = blank(&[&put(0u32)]);
                reply.reply_to_msg_id = Some(root - 1);
                reply.reply_to_top_id = Some(root);
                said.reply_to = Some(reply.into());
                if !self.discussion_left {
                    self.store(None, said.clone());
                }
                vec![discussion_update(tl::types::UpdateNewChannelMessage { message: said.into(), pts: 0, pts_count: 0 }.into(), false, true)]
            }
            // Telegram sends both a source update and a personal copy addressed to self.
            "discussion-reply" => {
                let source = self.steer("discussion-comment 60 82");
                let mut copy = message(Who::User(ME), 83, REPLIES, crate::hub::now() as i32, "A reply to your comment");
                let mut reply: tl::types::MessageReplyHeader = blank(&[&put(0u32)]);
                reply.reply_to_msg_id = Some(59);
                reply.reply_to_top_id = Some(ECHO);
                reply.reply_to_peer_id = Some(Who::Channel(CHATTERS).peer());
                copy.reply_to = Some(reply.into());
                let mut forward: tl::types::MessageFwdHeader = blank(&[&put(0u32), &put(t0())]);
                forward.from_id = Some(Who::User(TOBIN).peer());
                forward.saved_from_peer = Some(Who::Channel(CHATTERS).peer());
                forward.saved_from_msg_id = Some(82);
                copy.fwd_from = Some(forward.into());
                self.store(None, { let mut stored = copy.clone(); stored.peer_id = Who::User(REPLIES).peer(); stored });
                source.into_iter().chain([discussion_update(tl::types::UpdateNewMessage { message: copy.into(), pts: 0, pts_count: 0 }.into(), false, true)]).collect()
            }
            "edit" => {
                let mut message = message(odalys, 13, ODALYS, t0() + 10, "Café at the dock, seven sharp");
                message.edit_date = Some(t0() + 30);
                self.store(None, message.clone());
                vec![update(tl::types::UpdateEditMessage { message: message.into(), pts: 0, pts_count: 0 }.into())]
            }
            "recent-stickers" => {
                self.show_recent = true;
                Vec::new()
            }
            "featured-premium" => {
                self.featured_premium = true;
                Vec::new()
            }
            // Installed, and still on the Premium featured list, so a non-Premium account keeps it locked.
            "gold-installed" => {
                self.featured_premium = true;
                self.gold_installed = true;
                Vec::new()
            }
            "call-flags" => {
                let id = words.next().and_then(|id| id.parse().ok()).unwrap_or(ODALYS);
                let avail = words.next().is_some_and(|v| v == "1" || v == "true");
                let priv_ = words.next().is_some_and(|v| v == "1" || v == "true");
                let video = words.next().is_some_and(|v| v == "1" || v == "true");
                self.call_flags.insert(id, (avail, priv_, video));
                Vec::new()
            }
            "voice-forbidden" => {
                let id = words.next().and_then(|id| id.parse().ok()).unwrap_or(ODALYS);
                let on = words.next().map_or(true, |v| v == "1" || v == "true");
                if on {
                    self.voice_forbidden.insert(id);
                } else {
                    self.voice_forbidden.remove(&id);
                }
                Vec::new()
            }
            "voice-ban" => {
                if let Some((who, None)) = words.next().and_then(Who::parse) {
                    let on = words.next().map_or(true, |v| v == "1" || v == "true");
                    if on {
                        self.voice_banned.insert(who);
                    } else {
                        self.voice_banned.remove(&who);
                    }
                }
                Vec::new()
            }
            "transcribe-mode" => {
                self.transcribe_mode = words.next().unwrap_or("").to_owned();
                if let Some(trial) = words.next().and_then(|v| v.parse::<i32>().ok()) {
                    self.transcribe_trial = Some(trial);
                }
                Vec::new()
            }
            "privacy-refuse" => {
                self.privacy_refuse = words.next().map_or(true, |v| v != "0" && v != "off");
                Vec::new()
            }
            "autodownload-refuse" => {
                self.download_refuse = words.next().map_or(true, |v| v != "0" && v != "off");
                Vec::new()
            }
            "autodownload-unread" => {
                self.download_unread = words.next().map_or(true, |v| v != "0" && v != "off");
                Vec::new()
            }
            "boost-refuse" => {
                self.boost_refuse = words.next().map_or(true, |v| v != "0" && v != "off");
                Vec::new()
            }
            "start-refuse" => {
                self.start_refuse = words.next().map_or(true, |v| v != "0" && v != "off");
                Vec::new()
            }
            "transcribe-update" => {
                // transcribe-update <peer> <msg_id> <pending:0|1> <text...>
                let peer = words.next().and_then(Who::parse).map(|(w, _)| w).unwrap_or(odalys);
                let msg_id = words.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(20);
                let pending = words.next().is_some_and(|v| v == "1" || v == "true");
                let text = words.collect::<Vec<_>>().join(" ");
                vec![update(
                    tl::types::UpdateTranscribedAudio {
                        pending,
                        peer: peer.peer(),
                        msg_id,
                        transcription_id: 901,
                        text,
                    }
                    .into(),
                )]
            }
            "call-event" => {
                // call-event <out|in> <missed|busy|hangup|disconnect|none|group-live|group-end|conf-live|conf-missed> [video] [duration]
                let out = words.next() == Some("out");
                let kind = words.next().unwrap_or("missed");
                let video = words.next() == Some("video");
                let duration: Option<i32> = words.next().and_then(|d| d.parse().ok());
                let (peer, action): (Who, tl::enums::MessageAction) = match kind {
                    "group-live" => (
                        Who::Group(CREW),
                        tl::types::MessageActionGroupCall {
                            call: tl::types::InputGroupCall { id: 501, access_hash: 502 }.into(),
                            duration: None,
                        }
                        .into(),
                    ),
                    "group-end" => (
                        Who::Group(CREW),
                        tl::types::MessageActionGroupCall {
                            call: tl::types::InputGroupCall { id: 501, access_hash: 502 }.into(),
                            duration: Some(duration.unwrap_or(185)),
                        }
                        .into(),
                    ),
                    "conf-live" => (
                        Who::Group(CREW),
                        tl::types::MessageActionConferenceCall {
                            missed: false,
                            active: true,
                            video,
                            call_id: 601,
                            duration: None,
                            other_participants: None,
                        }
                        .into(),
                    ),
                    "conf-missed" => (
                        Who::Group(CREW),
                        tl::types::MessageActionConferenceCall {
                            missed: true,
                            active: false,
                            video,
                            call_id: 602,
                            duration: None,
                            other_participants: None,
                        }
                        .into(),
                    ),
                    other => {
                        let reason = match other {
                            "missed" => Some(tl::enums::PhoneCallDiscardReason::Missed),
                            "busy" => Some(tl::enums::PhoneCallDiscardReason::Busy),
                            "hangup" => Some(tl::enums::PhoneCallDiscardReason::Hangup),
                            "disconnect" => Some(tl::enums::PhoneCallDiscardReason::Disconnect),
                            _ => None,
                        };
                        (
                            odalys,
                            tl::types::MessageActionPhoneCall {
                                video,
                                call_id: i64::from(self.next) + 900,
                                reason,
                                duration,
                            }
                            .into(),
                        )
                    }
                };
                self.next += 1;
                let id = self.next;
                let from_id = if out { Some(Who::User(ME).peer()) } else { Some(Who::User(ODALYS).peer()) };
                let mut svc: tl::types::MessageService = blank(&[
                    &put(0u32),
                    &put(id),
                    &put(peer.peer()),
                    &put(t0() + id),
                    &put(action),
                ]);
                svc.out = out;
                svc.from_id = from_id;
                let raw: tl::enums::Message = svc.into();
                self.messages.entry(key(peer, None)).or_default().insert(id, raw.clone());
                vec![update(tl::types::UpdateNewMessage { message: raw, pts: 0, pts_count: 0 }.into())]
            }
            "peer-blocked" => {
                let id = words.next().and_then(|id| id.parse().ok()).unwrap_or(ODALYS);
                let blocked = words.next() != Some("off");
                if blocked {
                    self.blocked.insert(id);
                } else {
                    self.blocked.remove(&id);
                }
                vec![update(tl::types::UpdatePeerBlocked { blocked, blocked_my_stories_from: false, peer_id: Who::User(id).peer() }.into())]
            }
            // A live location, then the same message moved. The second is an edit, so it must
            // not count as another unread message.
            "live-place" | "live-move" => {
                let moved = line == "live-move";
                let (lat, long) = if moved { (37.8, -122.5) } else { (37.7, -122.4) };
                let mut message = message(odalys, 40, ODALYS, crate::hub::now() as i32, "");
                if moved {
                    message.edit_date = Some(crate::hub::now() as i32);
                }
                message.media = Some(
                    tl::types::MessageMediaGeoLive {
                        geo: tl::types::GeoPoint { long, lat, access_hash: 0, accuracy_radius: None }.into(),
                        heading: Some(90),
                        period: 3_600,
                        proximity_notification_radius: None,
                    }
                    .into(),
                );
                self.store(None, message.clone());
                if moved {
                    vec![update(tl::types::UpdateEditMessage { message: message.into(), pts: 0, pts_count: 0 }.into())]
                } else {
                    vec![new(message)]
                }
            }
            "delete" => {
                if let Some(held) = self.messages.get_mut(&odalys.text()) {
                    held.remove(&13);
                }
                vec![update(tl::types::UpdateDeleteMessages { messages: vec![13], pts: 0, pts_count: 1 }.into())]
            }
            // One message whose own timer ran out before it arrived, and one with a moment
            // still to live: the store never shows the first and drops the second at its
            // moment, whether the account is online or not.
            "ttl" => {
                let mut gone = message(odalys, 17, ODALYS, t0() - 3_600, "Gone before it arrived");
                gone.ttl_period = Some(60);
                let mut alive = message(odalys, 18, ODALYS, crate::hub::now() as i32, "Vanishes in a moment");
                alive.ttl_period = Some(10);
                self.store(None, gone.clone());
                self.store(None, alive.clone());
                vec![new(gone), new(alive)]
            }
            // Telegram's own word that both are gone after their time, as its server says to
            // everyone still listening.
            "ttl-gone" => {
                if let Some(held) = self.messages.get_mut(&odalys.text()) {
                    held.remove(&17);
                    held.remove(&18);
                }
                vec![update(tl::types::UpdateDeleteMessages { messages: vec![17, 18], pts: 0, pts_count: 2 }.into())]
            }
            "topic" => {
                let message = in_topic(message(Who::Channel(WORKSHOP), 42, TOBIN, t0() + 40, "Found it, bottom drawer."), TOOLS);
                self.store(Some(TOOLS), message.clone());
                vec![update(tl::types::UpdateNewChannelMessage { message: message.into(), pts: 0, pts_count: 0 }.into())]
            }
            "typing" => vec![update(
                tl::types::UpdateUserTyping { user_id: ODALYS, top_msg_id: None, action: tl::enums::SendMessageAction::SendMessageTypingAction }.into(),
            )],
            "record-audio" => vec![update(
                tl::types::UpdateUserTyping { user_id: ODALYS, top_msg_id: None, action: tl::enums::SendMessageAction::SendMessageRecordAudioAction }.into(),
            )],
            "record-round" => vec![update(
                tl::types::UpdateUserTyping { user_id: ODALYS, top_msg_id: None, action: tl::enums::SendMessageAction::SendMessageRecordRoundAction }.into(),
            )],
            "upload-photo" => vec![update(
                tl::types::UpdateUserTyping {
                    user_id: ODALYS,
                    top_msg_id: None,
                    action: tl::enums::SendMessageAction::SendMessageUploadPhotoAction(tl::types::SendMessageUploadPhotoAction { progress: 50 }),
                }
                .into(),
            )],
            "choose-sticker" => vec![update(
                tl::types::UpdateUserTyping { user_id: ODALYS, top_msg_id: None, action: tl::enums::SendMessageAction::SendMessageChooseStickerAction }.into(),
            )],
            "speaking" => vec![update(
                tl::types::UpdateUserTyping { user_id: ODALYS, top_msg_id: None, action: tl::enums::SendMessageAction::SpeakingInGroupCallAction }.into(),
            )],
            "typing-stop" => vec![update(
                tl::types::UpdateUserTyping { user_id: ODALYS, top_msg_id: None, action: tl::enums::SendMessageAction::SendMessageCancelAction }.into(),
            )],
            "read" => vec![update(
                tl::types::UpdateReadHistoryInbox { folder_id: None, peer: odalys.peer(), top_msg_id: None, max_id: 14, still_unread_count: 0, pts: 0, pts_count: 0 }.into(),
            )],
            // Another device set the group's manual unread mark, or `unread-mark off` cleared it.
            "unread-mark" => {
                let unread = words.next() != Some("off");
                let who = Who::Group(CREW);
                self.unread_marks.insert(who, unread);
                vec![update(
                    tl::types::UpdateDialogUnreadMark {
                        unread,
                        peer: tl::types::DialogPeer { peer: who.peer() }.into(),
                        saved_peer_id: None,
                    }
                    .into(),
                )]
            }
            // A draft typed on the phone while we are here, and one the phone dropped.
            "draft" => {
                let text = "Riding up with Tobin.".to_owned();
                self.drafts.insert((odalys, None), text.clone());
                vec![update(tl::types::UpdateDraftMessage { peer: odalys.peer(), top_msg_id: None, saved_peer_id: None, draft: draft_message(&text) }.into())]
            }
            "draft-none" => {
                self.drafts.remove(&(odalys, None));
                vec![update(
                    tl::types::UpdateDraftMessage { peer: odalys.peer(), top_msg_id: None, saved_peer_id: None, draft: tl::enums::DraftMessage::Empty(blank(&[&put(0u32)])) }.into(),
                )]
            }
            // A draft typed on the phone in the comments thread: Telegram keeps it in the
            // discussion group, under the echo of the post, as its own apps save one.
            "thread-draft" => {
                let text = "Ropes at the hut, then.".to_owned();
                self.drafts.insert((Who::Channel(CHATTERS), Some(ECHO)), text.clone());
                vec![update(tl::types::UpdateDraftMessage { peer: Who::Channel(CHATTERS).peer(), top_msg_id: Some(ECHO), saved_peer_id: None, draft: draft_message(&text) }.into())]
            }
            // Odalys goes offline; Tobin's last-seen stays hidden.
            "status" => vec![
                update(tl::types::UpdateUserStatus { user_id: ODALYS, status: tl::types::UserStatusOffline { was_online: t0() }.into() }.into()),
                update(tl::types::UpdateUserStatus { user_id: TOBIN, status: tl::enums::UserStatus::Recently(blank(&[&put(0u32)])) }.into()),
            ],
            "rename" => vec![update(
                tl::types::UpdateUserName {
                    user_id: TOBIN,
                    first_name: "Tobin".to_owned(),
                    last_name: "Hale".to_owned(),
                    usernames: vec![tl::types::Username { editable: true, active: true, username: "ridge".to_owned() }.into()],
                }
                .into(),
            )],
            "poll" => {
                let text = |text: &str| tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities { text: text.to_owned(), entities: Vec::new() });
                let poll = tl::types::Poll {
                    id: 1,
                    closed: false,
                    public_voters: false,
                    multiple_choice: false,
                    quiz: false,
                    open_answers: false,
                    revoting_disabled: false,
                    shuffle_answers: false,
                    hide_results_until_close: false,
                    creator: false,
                    subscribers_only: false,
                    question: text("Saturday or Sunday?"),
                    answers: Vec::new(),
                    close_period: None,
                    close_date: None,
                    countries_iso2: None,
                    hash: 0,
                };
                let results: tl::types::PollResults = blank(&[&put(0u32)]);
                let mut message = message(Who::Group(CREW), 21, TOBIN, t0() + 50, "");
                message.media = Some(tl::enums::MessageMedia::Poll(Box::new(tl::types::MessageMediaPoll { poll: poll.into(), results: results.into(), attached_media: None })));
                self.store(None, message.clone());
                vec![update(tl::types::UpdateNewMessage { message: message.into(), pts: 0, pts_count: 0 }.into())]
            }
            // The same question with its options and counts, then a full update, then a
            // results-only update whose option bytes cannot be lined up with the labels.
            "poll-full" => {
                let (poll, results) = crew_poll(&[(0, 3), (1, 1)], 4);
                let mut message = message(Who::Group(CREW), 31, TOBIN, t0() + 70, "");
                message.media = Some(tl::enums::MessageMedia::Poll(Box::new(tl::types::MessageMediaPoll { poll, results, attached_media: None })));
                self.store(None, message.clone());
                vec![update(tl::types::UpdateNewMessage { message: message.into(), pts: 0, pts_count: 0 }.into())]
            }
            "poll-update" => {
                let (poll, results) = crew_poll(&[(0, 5), (1, 2)], 7);
                vec![update(
                    tl::types::UpdateMessagePoll {
                        peer: Some(Who::Group(CREW).peer()),
                        msg_id: Some(31),
                        top_msg_id: None,
                        poll_id: 44,
                        poll: Some(poll),
                        results,
                    }
                    .into(),
                )]
            }
            "poll-counts" => {
                let (_, results) = crew_poll(&[(0, 9), (1, 9)], 18);
                vec![update(
                    tl::types::UpdateMessagePoll {
                        peer: Some(Who::Group(CREW).peer()),
                        msg_id: Some(31),
                        top_msg_id: None,
                        poll_id: 44,
                        poll: None,
                        results,
                    }
                    .into(),
                )]
            }
            // A link card with the row-sized still and a larger photo that a row does not fetch.
            "link" => {
                let mut page: tl::types::WebPage = blank(&[&put(0u32), &put(8_801i64), &put("https://example.org/ridge".to_owned()), &put("example.org".to_owned()), &put(1i32)]);
                page.site_name = Some("Example".to_owned());
                page.title = Some("Ridge".to_owned());
                page.description = Some("The trail.".to_owned());
                page.photo = Some(photo(8_801));
                let mut message = message(odalys, 32, ODALYS, t0() + 80, "https://example.org/ridge");
                message.media = Some(tl::types::MessageMediaWebPage {
                    force_large_media: false,
                    force_small_media: false,
                    manual: false,
                    safe: true,
                    webpage: page.into(),
                }.into());
                self.store(None, message.clone());
                vec![new(message)]
            }
            "voice" => {
                let voice = tl::types::Document {
                    id: 6_100,
                    access_hash: 6_101,
                    file_reference: vec![4],
                    date: t0(),
                    mime_type: "audio/ogg".to_owned(),
                    size: 18_000,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl::types::DocumentAttributeAudio {
                        voice: true,
                        duration: 7,
                        title: None,
                        performer: None,
                        waveform: Some(vec![0b0001_1111, 0b0111_1100, 0b1111_0000, 0b1100_0001, 0b0000_0111]),
                    }
                    .into()],
                };
                let mut message = message(odalys, 15, ODALYS, t0() + 60, "");
                message.media = Some(
                    tl::types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: true,
                        document: Some(voice.into()),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    }
                    .into(),
                );
                self.store(None, message.clone());
                vec![new(message)]
            }
            "voice-unread" => {
                let voice = tl::types::Document {
                    id: 6_101,
                    access_hash: 6_102,
                    file_reference: vec![4],
                    date: t0(),
                    mime_type: "audio/ogg".to_owned(),
                    size: 18_000,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl::types::DocumentAttributeAudio {
                        voice: true,
                        duration: 9,
                        title: None,
                        performer: None,
                        waveform: Some(vec![0b0001_1111, 0b0111_1100, 0b1111_0000]),
                    }
                    .into()],
                };
                let mut msg = message(odalys, 61, ODALYS, t0() + 61, "");
                msg.media_unread = true;
                msg.media = Some(
                    tl::types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: true,
                        document: Some(voice.into()),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    }
                    .into(),
                );
                self.store(None, msg.clone());
                vec![new(msg)]
            }
            "voice-unread-mention" => {
                let voice = tl::types::Document {
                    id: 6_102,
                    access_hash: 6_103,
                    file_reference: vec![4],
                    date: t0(),
                    mime_type: "audio/ogg".to_owned(),
                    size: 18_000,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl::types::DocumentAttributeAudio {
                        voice: true,
                        duration: 5,
                        title: None,
                        performer: None,
                        waveform: Some(vec![0b0001_1111, 0b0111_1100]),
                    }
                    .into()],
                };
                let mut msg = message(Who::Group(CREW), 62, TOBIN, t0() + 62, "");
                msg.media_unread = true;
                msg.mentioned = true;
                msg.media = Some(
                    tl::types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: true,
                        document: Some(voice.into()),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    }
                    .into(),
                );
                self.store(None, msg.clone());
                vec![new(msg)]
            }
            "voice-out-unread" => {
                let voice = tl::types::Document {
                    id: 6_103,
                    access_hash: 6_104,
                    file_reference: vec![4],
                    date: t0(),
                    mime_type: "audio/ogg".to_owned(),
                    size: 18_000,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl::types::DocumentAttributeAudio {
                        voice: true,
                        duration: 6,
                        title: None,
                        performer: None,
                        waveform: Some(vec![0b0001_1111, 0b0111_1100]),
                    }
                    .into()],
                };
                let mut msg = message(odalys, 63, ME, t0() + 63, "");
                msg.media_unread = true;
                msg.media = Some(
                    tl::types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: true,
                        document: Some(voice.into()),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    }
                    .into(),
                );
                self.store(None, msg.clone());
                vec![new(msg)]
            }
            "channel-voice-unread" => {
                let voice = tl::types::Document {
                    id: 6_104,
                    access_hash: 6_105,
                    file_reference: vec![4],
                    date: t0(),
                    mime_type: "audio/ogg".to_owned(),
                    size: 18_000,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl::types::DocumentAttributeAudio {
                        voice: true,
                        duration: 11,
                        title: None,
                        performer: None,
                        waveform: Some(vec![0b0001_1111, 0b0111_1100]),
                    }
                    .into()],
                };
                let mut msg = message(Who::Channel(NOTES), 64, 0, t0() + 64, "");
                msg.media_unread = true;
                msg.media = Some(
                    tl::types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: true,
                        document: Some(voice.into()),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    }
                    .into(),
                );
                self.store(None, msg.clone());
                vec![update(tl::types::UpdateNewChannelMessage { message: msg.into(), pts: 0, pts_count: 0 }.into())]
            }
            "round-unread" => {
                let round = tl::types::Document {
                    id: 6_105,
                    access_hash: 6_106,
                    file_reference: vec![4],
                    date: t0(),
                    mime_type: "video/mp4".to_owned(),
                    size: 42_000,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl::types::DocumentAttributeVideo {
                        round_message: true,
                        supports_streaming: true,
                        nosound: false,
                        duration: 8.0,
                        w: 240,
                        h: 240,
                        preload_prefix_size: None,
                        video_start_ts: None,
                        video_codec: None,
                    }
                    .into()],
                };
                let mut msg = message(odalys, 65, ODALYS, t0() + 65, "");
                msg.media_unread = true;
                msg.media = Some(
                    tl::types::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: true,
                        round: true,
                        voice: false,
                        document: Some(round.into()),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    }
                    .into(),
                );
                self.store(None, msg.clone());
                vec![new(msg)]
            }
            "read-contents" => {
                let ids: Vec<i32> = words.filter_map(|w| w.parse::<i32>().ok()).collect();
                vec![update(tl::types::UpdateReadMessagesContents { messages: ids, pts: 5, pts_count: 1, date: None }.into())]
            }
            "channel-read-contents" => {
                let channel_id = words.next().and_then(|w| w.parse::<i64>().ok()).unwrap_or(NOTES);
                let ids: Vec<i32> = words.filter_map(|w| w.parse::<i32>().ok()).collect();
                vec![update(
                    tl::types::UpdateChannelReadMessagesContents {
                        channel_id,
                        top_msg_id: None,
                        saved_peer_id: None,
                        messages: ids,
                    }
                    .into(),
                )]
            }
            "slowmode" => {
                let seconds = words.next().and_then(|w| w.parse::<i32>().ok()).unwrap_or(30);
                if seconds > 0 {
                    self.slowmode_seconds.insert(WORKSHOP, seconds);
                } else {
                    self.slowmode_seconds.remove(&WORKSHOP);
                }
                Vec::new()
            }
            "ban-media" => {
                self.media_banned.insert(WORKSHOP);
                Vec::new()
            }
            "paid-stars" => {
                let stars = words.next().and_then(|w| w.parse::<i64>().ok()).unwrap_or(25);
                if stars > 0 {
                    self.paid_stars.insert(TOBIN, stars);
                } else {
                    self.paid_stars.remove(&TOBIN);
                }
                Vec::new()
            }
            // datacenter does, and the first CDN read asks for the file to be sent again.
            "cdn-file" => {
                let document = plain_document(9_300, CDN_PLAIN.len() as i64, "ridge.bin");
                let mut message = message(odalys, 30, ODALYS, t0() + 80, "");
                message.media = Some(document_media(document));
                self.store(None, message.clone());
                vec![new(message)]
            }
            // Longer than one megabyte piece, so a drop in the middle leaves a part to resume.
            "big-file" => {
                let document = plain_document(9_400, BIG_FILE as i64, "ridge-map.bin");
                let mut message = message(odalys, 31, ODALYS, t0() + 90, "");
                message.media = Some(document_media(document));
                self.store(None, message.clone());
                vec![new(message)]
            }
            // A post from the channel, forwarded into the direct chat as an answer to message 12.
            "forward" => {
                let mut message = message(odalys, 16, ODALYS, t0() + 70, "Snow line is at 1,900 m this week.");
                let mut from: tl::types::MessageFwdHeader = blank(&[&put(0u32), &put(t0() - 7_200)]);
                from.from_id = Some(Who::Channel(NOTES).peer());
                from.channel_post = Some(5);
                message.fwd_from = Some(from.into());
                let mut reply: tl::types::MessageReplyHeader = blank(&[&put(0u32)]);
                reply.reply_to_msg_id = Some(12);
                message.reply_to = Some(reply.into());
                self.store(None, message.clone());
                vec![new(message)]
            }
            // A bot message with two rows of inline keyboard buttons (callback, URL, copy, switch_inline).
            "bot-buttons" => {
                let bot = Who::User(BOT);
                let mut msg = message(bot, 90, BOT, t0() + 80, "Choose an action:");
                let row1 = tl::types::KeyboardButtonRow {
                    buttons: vec![
                        tl::types::KeyboardButtonCallback {
                            requires_password: false,
                            style: None,
                            text: "Approve".to_owned(),
                            data: b"approve:42".to_vec(),
                        }
                        .into(),
                        tl::types::KeyboardButtonUrl {
                            style: None,
                            text: "Docs".to_owned(),
                            url: "https://example.com/docs".to_owned(),
                        }
                        .into(),
                    ],
                }
                .into();
                let row2 = tl::types::KeyboardButtonRow {
                    buttons: vec![
                        tl::types::KeyboardButtonCopy {
                            style: None,
                            text: "Copy code".to_owned(),
                            copy_text: "ABC-123".to_owned(),
                        }
                        .into(),
                        tl::types::KeyboardButtonSwitchInline {
                            same_peer: true,
                            style: None,
                            text: "Search".to_owned(),
                            query: "trails ".to_owned(),
                            peer_types: None,
                        }
                        .into(),
                    ],
                }
                .into();
                msg.reply_markup = Some(tl::types::ReplyInlineMarkup { rows: vec![row1, row2] }.into());
                self.store(None, msg.clone());
                vec![new(msg)]
            }
            // Three unread mentions and two unread reactions in Trail crew, one mention in the
            // Tools topic, and the channel post that already names the user. The messages stay
            // on the wire until a jump asks for them. The dialog mark is what puts the counts
            // on the row.
            "unread-jumps" => {
                let crew = Who::Group(CREW);
                let mut m21 = message(crew, 21, TOBIN, t0() + 10, "@mira_k bring the stove!");
                m21.mentioned = true;
                let mut m22 = message(crew, 22, ODALYS, t0() + 20, "@mira_k check the map!");
                m22.mentioned = true;
                let mut m23 = message(crew, 23, TOBIN, t0() + 30, "@mira_k the gate code!");
                m23.mentioned = true;
                let m24 = message(crew, 24, ME, t0() + 40, "Stove packed.");
                let m25 = message(crew, 25, ME, t0() + 50, "Map packed.");
                for m in [&m21, &m22, &m23, &m24, &m25] {
                    self.store(None, m.clone());
                }
                self.unread_mentions.insert(crew.text(), vec![21, 22, 23]);
                self.unread_reactions.insert(crew.text(), vec![24, 25]);
                let mut topic = in_topic(
                    message(Who::Channel(WORKSHOP), 42, TOBIN, t0() + 60, "@mira_k the wrench is back."),
                    TOOLS,
                );
                topic.mentioned = true;
                self.store(Some(TOOLS), topic);
                self.unread_mentions.insert(key(Who::Channel(WORKSHOP), Some(TOOLS)), vec![42]);
                self.unread_mentions.insert(Who::Channel(NOTES).text(), vec![5]);
                vec![update(
                    tl::types::UpdateDialogUnreadMark {
                        unread: false,
                        peer: tl::types::DialogPeer { peer: crew.peer() }.into(),
                        saved_peer_id: None,
                    }
                    .into(),
                )]
            }
            // The channel's post gains a section of comments: the post says its count and the
            // linked group it is answered in, and two comments stand in the thread. A number
            // after the word says how many of them the user has not read.
            "comments" => {
                self.comments_unread = words.next().and_then(|count| count.parse().ok()).unwrap_or(0);
                let mut post = message(Who::Channel(NOTES), 5, 0, t0() - 7_200, "Snow line is at 1,900 m this week.");
                post.replies = Some(blank::<tl::types::MessageReplies>(&[&put(1u32), &put(2i32), &put(0i32), &put(CHATTERS)]).into());
                self.store(None, post.clone());
                // A comment is a message of the discussion group, which `getReplies` answers
                // for the post itself, so it is kept under the post's thread here.
                let thread = self.messages.entry(key(Who::Channel(NOTES), Some(5))).or_default();
                for (id, from, date, text) in [(ECHO + 1, TOBIN, t0() - 100, "On my way up front?"), (NEWEST_COMMENT, ODALYS, t0() - 40, "Yes, at the hut now.")] {
                    thread.insert(id, message(Who::Channel(CHATTERS), id, from, date, text).into());
                }
                vec![update(tl::types::UpdateEditChannelMessage { message: post.into(), pts: 0, pts_count: 0 }.into())]
            }
            // A service line and a kicked typing signal in the group.
            "service" => {
                let service = tl::types::MessageService {
                    out: false,
                    mentioned: false,
                    media_unread: false,
                    reactions_are_possible: false,
                    silent: false,
                    post: false,
                    legacy: false,
                    id: 22,
                    from_id: Some(Who::User(TOBIN).peer()),
                    peer_id: Who::Group(CREW).peer(),
                    saved_peer_id: None,
                    reply_to: None,
                    date: t0() + 80,
                    action: tl::types::MessageActionChatEditTitle { title: "Ridge crew".to_owned() }.into(),
                    reactions: None,
                    ttl_period: None,
                };
                vec![update(tl::types::UpdateNewMessage { message: service.into(), pts: 0, pts_count: 0 }.into())]
            }
            // Telegram's own apps pinned the group; then reordered the pins without saying how.
            "pin" => vec![update(tl::types::UpdateDialogPinned { pinned: true, folder_id: None, peer: tl::types::DialogPeer { peer: Who::Group(CREW).peer() }.into() }.into())],
            "pins" => vec![update(tl::types::UpdatePinnedDialogs { folder_id: None, order: None }.into())],
            // Tobin was added to the Family folder.
            "folder" => {
                self.family_grew = true;
                vec![update(tl::types::UpdateDialogFilter { id: FAMILY, filter: Some(self.folder(FAMILY)) }.into())]
            }
            // The Work folder's rules changed: it takes groups by kind and leaves read ones out.
            "exclude-read" => {
                self.exclude_read = true;
                vec![update(tl::types::UpdateDialogFilter { id: WORK, filter: Some(self.folder(WORK)) }.into())]
            }
            // The forum was read, here or on the phone: nothing in it is unread.
            "read-forum" => vec![update(tl::types::UpdateReadChannelInbox {
                folder_id: None,
                channel_id: WORKSHOP,
                max_id: 41,
                still_unread_count: 0,
                pts: 0,
            }
            .into())],
            // A message that names me: in the group ("mention group"), in the archived chat
            // (bare "mention", which then reaches the Inbox and the rail's count for it), or
            // in the muted channel ("mention channel" — the mention count rides the dialog,
            // and the rail hears nothing of any of it).
            "mention" => {
                match words.next() {
                    Some("channel") => {
                        // A post in the muted channel names the user. The count rides the
                        // dialog, so Telegram marks the chats stale (a default said changed)
                        // and the next opening of the list reads it in.
                        self.channel_mention = true;
                        vec![update(tl::types::UpdateNotifySettings { peer: tl::enums::NotifyPeer::NotifyBroadcasts, notify_settings: settings(None) }.into())]
                    }
                    Some("group") => {
                        let mut said = message(Who::Group(CREW), 21, TOBIN, t0() + 90, "Are you seeing this?");
                        said.mentioned = true;
                        self.store(None, said.clone());
                        vec![new(said)]
                    }
                    _ => {
                        let mut said = message(Who::User(TOBIN), 32, TOBIN, t0() + 90, "Are you seeing this?");
                        said.mentioned = true;
                        self.store(None, said.clone());
                        vec![new(said)]
                    }
                }
            }
            // Odalys (or the chat the line names) read my messages up to this one.
            "outbox" => {
                let (who, id) = match words.next() {
                    Some(chat) if chat.parse::<i32>().is_ok() => (odalys.clone(), chat.to_owned()),
                    Some(chat) => (Who::parse(chat).map(|(who, _)| who).unwrap_or(odalys.clone()), words.next().unwrap_or("0").to_owned()),
                    None => (odalys.clone(), "0".to_owned()),
                };
                let max_id = id.parse().ok().unwrap_or(0);
                vec![update(tl::types::UpdateReadHistoryOutbox { peer: who.peer(), max_id, pts: 0, pts_count: 0 }.into())]
            }
            "flood-send" => {
                self.flood_send = words.next().and_then(|seconds| seconds.parse().ok());
                Vec::new()
            }
            "flood-file" => {
                self.flood_file = words.next().and_then(|seconds| seconds.parse().ok());
                Vec::new()
            }
            // N photo posts in the channel and one photo in the group, where the next page read
            // finds them, with no update: as posts made while the app was closed, their
            // thumbnails are asked for only once a page names them.
            "photo-pages" => {
                let many: i32 = words.next().and_then(|many| many.parse().ok()).unwrap_or(20);
                let media = |photo| Some(tl::types::MessageMediaPhoto { spoiler: false, live_photo: false, photo: Some(photo), ttl_seconds: None, video: None }.into());
                for at in 0..many {
                    let mut post = message(Who::Channel(NOTES), 200 + at, 0, t0() - 3_600 + at, &format!("Ridge photo {}", at + 1));
                    post.media = media(photo(PAGE_PHOTOS + i64::from(at)));
                    self.store(None, post);
                }
                let mut shared = message(Who::Group(CREW), 90, TOBIN, t0() - 1_800, "The pass this morning");
                shared.media = media(crew_photo());
                self.store(None, shared);
                Vec::new()
            }
            // The group's read never reached here, as when the tell went out while the link was
            // down: its dialog counts unread again until that read is told again.
            "lost-read" => {
                self.lost_reads = true;
                Vec::new()
            }
            // A default of the notify settings changed: what of it stands elsewhere stands, and
            // the chats are read again only when the user opens them.
            "notify-changed" => vec![update(
                tl::types::UpdateNotifySettings { peer: tl::enums::NotifyPeer::NotifyBroadcasts, notify_settings: settings(None) }.into(),
            )],
            // The link goes down and chatd reconnects when it is back, without the session
            // earning its end: what Telegram carried before stands on the list.
            "drop" => vec![Event::Closed("closed".to_owned())],
            "revoke" => {
                self.revoked = true;
                vec![Event::Closed("the session was ended from another device".to_owned())]
            }
            // Odalys asked for a secret chat. The adapter names the row and does not accept it.
            "secret" => {
                let chat = tl::types::EncryptedChatRequested {
                    folder_id: None,
                    id: 42,
                    access_hash: 1,
                    date: t0(),
                    admin_id: ODALYS,
                    participant_id: ME,
                    g_a: vec![1, 2, 3],
                };
                vec![update(tl::types::UpdateEncryption { chat: chat.into(), date: t0() }.into())]
            }
            // Telegram says that chat is gone. The row leaves with it.
            "secret-gone" => {
                let chat = tl::types::EncryptedChatDiscarded { history_deleted: false, id: 42 };
                vec![update(tl::types::UpdateEncryption { chat: chat.into(), date: t0() }.into())]
            }
            _ => Vec::new(),
        }
    }

    fn handle(&mut self, body: &[u8], events: &Sender<Event>) -> Result<Vec<u8>, Fault> {
        use tl::functions as f;
        // Signing in.
        if asked::<f::auth::ExportLoginToken>(body).is_some() {
            self.log("ExportLoginToken");
            return match self.scanned.take() {
                Some(false) => {
                    self.signed_in = true;
                    answer(tl::enums::auth::LoginToken::Success(tl::types::auth::LoginTokenSuccess { authorization: self.me() }))
                }
                Some(true) => {
                    self.password_owed = true;
                    Err(rpc(401, "SESSION_PASSWORD_NEEDED", None))
                }
                None => {
                    self.tokens += 1;
                    let seconds = std::env::var("TINYCLIENT_TELEGRAM_FAKE").ok().and_then(|seconds| seconds.parse::<i64>().ok()).unwrap_or(30);
                    let token = tl::types::auth::LoginToken { expires: (crate::hub::now() + seconds) as i32, token: vec![self.tokens; 32] };
                    answer(tl::enums::auth::LoginToken::Token(token))
                }
            };
        }
        if let Some(request) = asked::<f::auth::SendCode>(body) {
            self.log("SendCode");
            let sms = |timeout| {
                tl::enums::auth::SentCode::Code(tl::types::auth::SentCode {
                    r#type: tl::types::auth::SentCodeTypeApp { length: 5 }.into(),
                    phone_code_hash: "hash-1".to_owned(),
                    next_type: Some(tl::enums::auth::CodeType::Sms),
                    timeout,
                })
            };
            return match request.phone_number.as_str() {
                "+15550100177" => {
                    self.phone = Some(request.phone_number);
                    self.has_recovery = false;
                    self.signup_required = false;
                    answer(tl::enums::auth::SentCode::Code(tl::types::auth::SentCode {
                        r#type: tl::types::auth::SentCodeTypeSetUpEmailRequired { apple_signin_allowed: false, google_signin_allowed: false }.into(),
                        phone_code_hash: "hash-mail".to_owned(),
                        next_type: None,
                        timeout: None,
                    }))
                }
                "+15550100142" | "+15550100199" | "+15550100166" | "+15550100188" => {
                    self.phone = Some(request.phone_number.clone());
                    self.has_recovery = request.phone_number == "+15550100166";
                    self.signup_required = request.phone_number == "+15550100188";
                    answer(sms(Some(60)))
                }
                "+15550100666" => Err(rpc(400, "PHONE_NUMBER_BANNED", None)),
                "+15550100420" => Err(rpc(420, "FLOOD_WAIT", Some(3_600))),
                _ => Err(rpc(400, "PHONE_NUMBER_INVALID", None)),
            };
        }
        if asked::<f::auth::ResendCode>(body).is_some() {
            self.log("ResendCode");
            return answer(tl::enums::auth::SentCode::Code(tl::types::auth::SentCode {
                r#type: tl::types::auth::SentCodeTypeSms { length: 5 }.into(),
                phone_code_hash: "hash-2".to_owned(),
                next_type: None,
                timeout: None,
            }));
        }
        if let Some(request) = asked::<f::auth::SignIn>(body) {
            // What was typed is not written to the log, here as anywhere.
            self.log("SignIn");
            return match (request.phone_code.as_deref(), self.phone.as_deref()) {
                (Some(CODE), Some("+15550100199" | "+15550100166")) => {
                    self.password_owed = true;
                    Err(rpc(401, "SESSION_PASSWORD_NEEDED", None))
                }
                (Some(CODE), Some("+15550100188")) if self.signup_required => answer(self.signup_required_auth()),
                (Some(CODE), Some(_)) => {
                    self.signed_in = true;
                    answer(self.me())
                }
                (Some("00000"), _) => Err(rpc(400, "PHONE_CODE_EXPIRED", None)),
                _ => Err(rpc(400, "PHONE_CODE_INVALID", None)),
            };
        }
        if let Some(request) = asked::<f::account::SendVerifyEmailCode>(body) {
            self.log("SendVerifyEmailCode");
            if !request.email.contains('@') || request.email.contains(char::is_whitespace) {
                return Err(rpc(400, "EMAIL_INVALID", None));
            }
            self.pending_login_email = Some(request.email.clone());
            let pattern = mask_email(&request.email);
            return answer(tl::enums::account::SentEmailCode::Code(tl::types::account::SentEmailCode { email_pattern: pattern, length: 6 }));
        }
        if let Some(request) = asked::<f::account::VerifyEmail>(body) {
            self.log("VerifyEmail");
            let tl::enums::EmailVerification::Code(proof) = request.verification else {
                return Err(rpc(400, "CODE_INVALID", None));
            };
            if proof.code != "654321" {
                return Err(rpc(400, "CODE_INVALID", None));
            }
            let email = self.pending_login_email.clone().unwrap_or_default();
            return answer(tl::enums::account::EmailVerified::Login(tl::types::account::EmailVerifiedLogin {
                email,
                sent_code: tl::types::auth::SentCode {
                    r#type: tl::types::auth::SentCodeTypeApp { length: 5 }.into(),
                    phone_code_hash: "hash-1".to_owned(),
                    next_type: Some(tl::enums::auth::CodeType::Sms),
                    timeout: Some(60),
                }
                .into(),
            }));
        }
        if let Some(request) = asked::<f::auth::SignUp>(body) {
            self.log("SignUp");
            if request.first_name.trim().is_empty() {
                return Err(rpc(400, "FIRSTNAME_INVALID", None));
            }
            self.signed_in = true;
            self.signup_required = false;
            return answer(self.me());
        }
        // Neither call reads the code or signs anyone in. `auth.recoverPassword` with no new
        // settings deletes the cloud password, and that recovery stays unimplemented (#542).
        if asked::<f::auth::RequestPasswordRecovery>(body).is_some() {
            self.log("RequestPasswordRecovery");
            return Err(rpc(400, "PASSWORD_RECOVERY_NA", None));
        }
        if asked::<f::auth::RecoverPassword>(body).is_some() {
            self.log("RecoverPassword");
            return Err(rpc(400, "PASSWORD_RECOVERY_NA", None));
        }
        if asked::<f::account::GetPassword>(body).is_some() {
            self.log("GetPassword");
            return answer(tl::enums::account::Password::Password(self.password()));
        }
        if let Some(request) = asked::<f::auth::CheckPassword>(body) {
            self.log("CheckPassword");
            let tl::enums::InputCheckPasswordSrp::Srp(proof) = request.password else {
                return Err(rpc(400, "PASSWORD_HASH_INVALID", None));
            };
            // The proof the right password gives, made the way a client makes it. grammers'
            // SRP bends a debug build's 256 KiB connection stack past its end, so the
            // computation runs on its own thread with room to spare.
            let asked = self.password();
            let secure_random = asked.secure_random;
            let srp = std::thread::Builder::new().name("fake srp".into()).stack_size(4 * 1024 * 1024).spawn(move || {
                calculate_2fa(&[7; 8], &[9; 16], &unhex(PRIME), &3, vec![5; 256], secure_random, PASSWORD.as_bytes())
            })
            .expect("the fake's SRP thread");
            let (m1, g_a) = srp.join().expect("the fake's SRP thread");
            return match self.password_owed && proof.srp_id == 77 && proof.m1 == m1 && proof.a == g_a {
                true => {
                    self.signed_in = true;
                    self.password_owed = false;
                    answer(self.me())
                }
                false => Err(rpc(400, "PASSWORD_HASH_INVALID", None)),
            };
        }
        if asked::<f::auth::LogOut>(body).is_some() {
            self.log("LogOut");
            self.signed_in = false;
            return answer(tl::enums::auth::LoggedOut::Out(blank(&[&put(0u32)])));
        }
        if self.revoked || !self.signed_in {
            return Err(rpc(401, "AUTH_KEY_UNREGISTERED", None));
        }
        // Signed in.
        // The account's drafts and a save of one are answered, like the pickers' asks below,
        // out of a frame of their own.
        if let Some(answered) = self.draft_asks(body, events) {
            return answered;
        }
        if asked::<f::updates::GetState>(body).is_some() {
            self.log("GetState");
            return answer(tl::enums::updates::State::State(tl::types::updates::State { pts: 1, qts: 0, date: t0(), seq: 1, unread_count: 0 }));
        }
        if asked::<f::users::GetUsers>(body).is_some() {
            self.log("GetUsers");
            return answer(vec![tl::enums::User::from(self.self_user())]);
        }
        if let Some(request) = asked::<f::account::GetNotifySettings>(body) {
            self.log("GetNotifySettings");
            let mute = match request.peer {
                tl::enums::InputNotifyPeer::InputNotifyUsers => self.notify_defaults[0].then_some(i32::MAX),
                tl::enums::InputNotifyPeer::InputNotifyChats => self.notify_defaults[1].then_some(i32::MAX),
                tl::enums::InputNotifyPeer::InputNotifyBroadcasts => self.notify_defaults[2].then_some(i32::MAX),
                _ => None,
            };
            return answer(settings(mute));
        }
        if asked::<f::account::GetNotifyExceptions>(body).is_some() {
            self.log("GetNotifyExceptions");
            let list: Vec<tl::enums::Update> = self
                .notify_exceptions
                .iter()
                .map(|(who, muted)| {
                    tl::types::UpdateNotifySettings {
                        peer: tl::types::NotifyPeer { peer: who.peer() }.into(),
                        notify_settings: settings(muted.then_some(i32::MAX)),
                    }
                    .into()
                })
                .collect();
            return answer(updates(list));
        }
        if asked::<f::account::ResetNotifySettings>(body).is_some() {
            self.log("ResetNotifySettings");
            self.notify_defaults = [false; 3];
            self.notify_exceptions.clear();
            return answer(true);
        }
        if asked::<f::account::GetAuthorizations>(body).is_some() {
            self.log("GetAuthorizations");
            let authorizations: Vec<tl::enums::Authorization> = self
                .sessions
                .iter()
                .map(|&(hash, app, device, region, country, current)| {
                    tl::types::Authorization {
                        current,
                        official_app: !current,
                        password_pending: false,
                        encrypted_requests_disabled: false,
                        call_requests_disabled: false,
                        unconfirmed: false,
                        hash,
                        device_model: device.to_owned(),
                        platform: "Desktop".to_owned(),
                        system_version: "1.0".to_owned(),
                        api_id: 1,
                        app_name: app.to_owned(),
                        app_version: "1.0".to_owned(),
                        date_created: t0() - 86_400 * 10,
                        date_active: t0() - 60,
                        ip: "127.0.0.1".to_owned(),
                        country: country.to_owned(),
                        region: region.to_owned(),
                    }
                    .into()
                })
                .collect();
            return answer(tl::enums::account::Authorizations::Authorizations(
                tl::types::account::Authorizations {
                    authorization_ttl_days: 180,
                    authorizations,
                },
            ));
        }
        if let Some(request) = asked::<f::account::ResetAuthorization>(body) {
            self.log(&format!("ResetAuthorization {}", request.hash));
            self.sessions.retain(|s| s.0 != request.hash || s.5);
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::GetUnreadMentions>(body) {
            let who = input_who(&request.peer);
            let k = key(who, request.top_msg_id);
            self.log(&format!("GetUnreadMentions {k}"));
            let ids = limited(self.unread_mentions.get(&k).map(Vec::as_slice).unwrap_or(&[]), request.limit);
            let msgs: Vec<tl::enums::Message> = ids
                .into_iter()
                .filter_map(|id| self.messages.get(&k).and_then(|m| m.get(&id)).cloned())
                .collect();
            return answer(tl::enums::messages::Messages::Messages(tl::types::messages::Messages {
                messages: msgs,
                topics: Vec::new(),
                chats: chats(),
                users: users(),
            }));
        }
        if let Some(request) = asked::<f::messages::ReadMessageContents>(body) {
            let joined = request.id.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
            self.log(&format!("ReadMessageContents {joined}"));
            for list in self.unread_mentions.values_mut() {
                list.retain(|id| !request.id.contains(id));
            }
            return answer(tl::enums::messages::AffectedMessages::Messages(
                tl::types::messages::AffectedMessages { pts: 1, pts_count: 0 },
            ));
        }
        if let Some(request) = asked::<f::channels::ReadMessageContents>(body) {
            let joined = request.id.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
            self.log(&format!("channels.ReadMessageContents {joined}"));
            for list in self.unread_mentions.values_mut() {
                list.retain(|id| !request.id.contains(id));
            }
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::ReadMentions>(body) {
            let who = input_who(&request.peer);
            let k = key(who, request.top_msg_id);
            self.log(&format!("ReadMentions {k}"));
            self.unread_mentions.remove(&k);
            return answer(tl::enums::messages::AffectedHistory::History(
                tl::types::messages::AffectedHistory { pts: 1, pts_count: 0, offset: 0 },
            ));
        }
        if let Some(request) = asked::<f::messages::GetUnreadReactions>(body) {
            let who = input_who(&request.peer);
            let k = key(who, request.top_msg_id);
            self.log(&format!("GetUnreadReactions {k}"));
            let ids = limited(self.unread_reactions.get(&k).map(Vec::as_slice).unwrap_or(&[]), request.limit);
            let msgs: Vec<tl::enums::Message> = ids
                .into_iter()
                .filter_map(|id| self.messages.get(&k).and_then(|m| m.get(&id)).cloned())
                .collect();
            return answer(tl::enums::messages::Messages::Messages(tl::types::messages::Messages {
                messages: msgs,
                topics: Vec::new(),
                chats: chats(),
                users: users(),
            }));
        }
        if let Some(request) = asked::<f::messages::ReadReactions>(body) {
            let who = input_who(&request.peer);
            let k = key(who, request.top_msg_id);
            self.log(&format!("ReadReactions {k}"));
            self.unread_reactions.remove(&k);
            return answer(tl::enums::messages::AffectedHistory::History(
                tl::types::messages::AffectedHistory { pts: 1, pts_count: 0, offset: 0 },
            ));
        }
        if asked::<f::messages::GetSavedDialogs>(body).is_some() {
            self.log("GetSavedDialogs");
            let m_odalys = message(Who::User(ME), 201, ME, t0() - 50, "Trailhead at seven.");
            let m_notes = message(Who::User(ME), 202, ME, t0() - 90, "Snow line is at 1,900 m this week.");
            let d1 = tl::types::SavedDialog {
                pinned: true,
                peer: Who::User(ODALYS).peer(),
                top_message: 201,
            }
            .into();
            let d2 = tl::types::SavedDialog {
                pinned: false,
                peer: Who::Channel(NOTES).peer(),
                top_message: 202,
            }
            .into();
            let found = tl::types::messages::SavedDialogs {
                dialogs: vec![d1, d2],
                messages: vec![m_odalys.into(), m_notes.into()],
                chats: chats(),
                users: users(),
            };
            return answer(tl::enums::messages::SavedDialogs::Dialogs(found));
        }
        if asked::<f::messages::GetSavedReactionTags>(body).is_some() {
            self.log("GetSavedReactionTags");
            let tag = tl::types::SavedReactionTag {
                reaction: tl::enums::Reaction::Emoji(tl::types::ReactionEmoji { emoticon: "⭐".to_owned() }),
                title: Some("Gear".to_owned()),
                count: 2,
            }
            .into();
            return answer(tl::enums::messages::SavedReactionTags::Tags(tl::types::messages::SavedReactionTags {
                tags: vec![tag],
                hash: 1,
            }));
        }
        if let Some(request) = asked::<f::messages::GetSavedHistory>(body) {
            let who = input_who(&request.peer);
            self.log(&format!("GetSavedHistory {}", who.text()));
            let msgs = match who {
                Who::User(ODALYS) => vec![message(Who::User(ME), 201, ME, t0() - 50, "Trailhead at seven.").into()],
                Who::Channel(NOTES) => vec![message(Who::User(ME), 202, ME, t0() - 90, "Snow line is at 1,900 m this week.").into()],
                _ => vec![message(Who::User(ME), 2, ME, t0() - 100_000, "Packing list: stove, map, headlamp.").into()],
            };
            return answer(tl::enums::messages::Messages::Messages(tl::types::messages::Messages {
                messages: msgs,
                topics: Vec::new(),
                chats: chats(),
                users: users(),
            }));
        }
        if asked::<f::account::UpdateStatus>(body).is_some() {
            self.log("UpdateStatus");
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::GetBotCallbackAnswer>(body) {
            let who = input_who(&request.peer);
            let data_str = request
                .data
                .as_ref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default();
            self.log(&format!("GetBotCallbackAnswer {} {} {}", who.text(), request.msg_id, data_str));
            return answer(tl::enums::messages::BotCallbackAnswer::Answer(tl::types::messages::BotCallbackAnswer {
                alert: false,
                has_url: false,
                native_ui: false,
                message: Some("Approved!".to_owned()),
                url: None,
                cache_time: 0,
            }));
        }
        if let Some(request) = asked::<f::messages::GetDialogs>(body) {
            let after = match &request.offset_peer {
                tl::enums::InputPeer::Empty => None,
                peer => Some(input_who(peer)),
            };
            self.log(&format!("GetDialogs {} {}", request.folder_id.unwrap_or(0), after.map_or_else(|| "start".to_owned(), Who::text)));
            let all: Vec<(tl::enums::Dialog, Who, String)> = match request.folder_id {
                Some(1) => vec![(dialog(Who::User(TOBIN), 31, 31, 31, 0, false, Some(1)), Who::User(TOBIN), Who::User(TOBIN).text())],
                _ => {
                    let mut all = vec![
                        (dialog(Who::User(ODALYS), 12, 11, 12, 1, true, None), Who::User(ODALYS), Who::User(ODALYS).text()),
                        {
                            let (read, unread) = self.crew();
                            (dialog(Who::Group(CREW), 20, read, 20, unread, false, None), Who::Group(CREW), Who::Group(CREW).text())
                        },
                        (dialog(Who::Channel(NOTES), 5, 0, 5, 1, false, None), Who::Channel(NOTES), Who::Channel(NOTES).text()),
                        (dialog(Who::Channel(WORKSHOP), 41, 0, 41, 1, false, None), Who::Channel(WORKSHOP), key(Who::Channel(WORKSHOP), Some(TOOLS))),
                        (dialog(Who::User(ME), 2, 2, 2, 0, false, None), Who::User(ME), Who::User(ME).text()),
                        (dialog(Who::User(SERVICE), 3, 3, 3, 0, false, None), Who::User(SERVICE), Who::User(SERVICE).text()),
                        // The bot has not read my last word to it: its read-outbox stops below.
                        (dialog(Who::User(BOT), 5, 5, 4, 0, false, None), Who::User(BOT), Who::User(BOT).text()),
                        (dialog(Who::User(SCAM), 6, 6, 6, 0, false, None), Who::User(SCAM), Who::User(SCAM).text()),
                    ];
                    if self.channel_mention {
                        for held in all.iter_mut().filter(|(_, who, _)| *who == Who::Channel(NOTES)) {
                            if let tl::enums::Dialog::Dialog(plain) = &mut held.0 {
                                plain.unread_mentions_count = 1;
                            }
                        }
                    }
                    for at in 0..crowd() {
                        let who = Who::User(CROWD + at);
                        all.push((dialog(who, 1, 1, 1, 0, false, None), who, who.text()));
                    }
                    // The impostor's chat sits past the crowd, so the paging tests' rows land
                    // where their arithmetic says: five pages end at crowd chat 492 exactly.
                    all.push((dialog(Who::User(FAKE), 7, 7, 7, 0, false, None), Who::User(FAKE), Who::User(FAKE).text()));
                    if self.off_page_private {
                        let who = Who::Channel(OFF_PAGE_GROUP);
                        all.push((self.off_page_dialog(), who, who.text()));
                    }
                    if lost_pin() {
                        // The pinned chat is no longer in the pages at all.
                        all.retain(|held| held.1 != Who::Group(CREW));
                    }
                    all
                }
            };
            let start = after.and_then(|after| all.iter().position(|(_, who, _)| *who == after)).map_or(0, |at| at + 1);
            let limit = request.limit.max(0) as usize;
            let page: Vec<&(tl::enums::Dialog, Who, String)> = all.iter().skip(start).take(limit).collect();
            let more = start + page.len() < all.len();
            let newest = |place: &str| self.messages.get(place).and_then(|held| held.values().next_back().cloned());
            let messages = page.iter().filter_map(|(_, _, place)| newest(place)).collect();
            let dialogs = page.iter().map(|(dialog, who, _)| {
                let mut dialog = dialog.clone();
                drafted(self, &mut dialog, *who);
                self.flag_unread(&mut dialog);
                if let tl::enums::Dialog::Dialog(plain) = &mut dialog {
                    plain.ttl_period = self.ttl_periods.get(who).copied().filter(|&p| p > 0);
                }
                dialog
            }).collect();
            let mut chats = chats();
            if page.iter().any(|(_, who, _)| *who == Who::Channel(OFF_PAGE_GROUP)) {
                let mut group = off_page_group();
                group.username = None;
                chats.push(group.into());
            }
            return answer(match more {
                true => tl::enums::messages::Dialogs::Slice(tl::types::messages::DialogsSlice { count: all.len() as i32, dialogs, messages, chats, users: users() }),
                false => tl::enums::messages::Dialogs::Dialogs(tl::types::messages::Dialogs { dialogs, messages, chats, users: users() }),
            });
        }
        if asked::<f::messages::GetDialogFilters>(body).is_some() {
            self.log("GetDialogFilters");
            let mut filters = vec![tl::enums::DialogFilter::Default];
            if let Some(custom) = &self.custom_folders {
                filters.extend(custom.iter().cloned());
            } else {
                filters.push(self.folder(WORK));
                filters.push(self.folder(FAMILY));
            }
            return answer(tl::enums::messages::DialogFilters::Filters(tl::types::messages::DialogFilters { tags_enabled: false, filters }));
        }
        if let Some(request) = asked::<f::messages::UpdateDialogFilter>(body) {
            if self.custom_folders.is_none() {
                let defaults = vec![self.folder(WORK), self.folder(FAMILY)];
                self.custom_folders = Some(defaults);
            }
            let id_of = |f: &tl::enums::DialogFilter| match f {
                tl::enums::DialogFilter::Filter(f) => Some(f.id),
                tl::enums::DialogFilter::Chatlist(c) => Some(c.id),
                tl::enums::DialogFilter::Default => None,
            };
            match request.filter {
                None => {
                    self.log(&format!("UpdateDialogFilter {} delete", request.id));
                    self.custom_folders.as_mut().unwrap().retain(|f| id_of(f) != Some(request.id));
                    return answer(true);
                }
                Some(filter) => {
                    let (title_text, empty_include) = match &filter {
                        tl::enums::DialogFilter::Filter(f) => {
                            let tl::enums::TextWithEntities::Entities(t) = &f.title;
                            let any_kind = f.contacts || f.non_contacts || f.groups || f.broadcasts || f.bots;
                            let any_peer = !f.include_peers.is_empty() || !f.pinned_peers.is_empty();
                            (t.text.clone(), !any_kind && !any_peer)
                        }
                        tl::enums::DialogFilter::Chatlist(c) => {
                            let tl::enums::TextWithEntities::Entities(t) = &c.title;
                            (t.text.clone(), c.include_peers.is_empty() && c.pinned_peers.is_empty())
                        }
                        tl::enums::DialogFilter::Default => (String::new(), true),
                    };
                    if title_text.trim().is_empty() {
                        return Err(rpc(400, "FILTER_TITLE_EMPTY", None));
                    }
                    if empty_include {
                        return Err(rpc(400, "FILTER_INCLUDE_EMPTY", None));
                    }
                    let existing = self.custom_folders.as_ref().unwrap().iter().position(|f| id_of(f) == Some(request.id));
                    let count = self.custom_folders.as_ref().unwrap().len();
                    let premium = std::env::var("TINYCLIENT_TELEGRAM_PREMIUM").ok().as_deref() == Some("1");
                    let limit = if premium { 30 } else { 10 };
                    if existing.is_none() && count >= limit {
                        self.log(&format!("UpdateDialogFilter {} refused FILTERS_TOO_MUCH", request.id));
                        return Err(rpc(400, "FILTERS_TOO_MUCH", None));
                    }
                    self.log(&format!("UpdateDialogFilter {} {}", request.id, title_text));
                    let folders = self.custom_folders.as_mut().unwrap();
                    if let Some(pos) = existing {
                        folders[pos] = filter;
                    } else {
                        folders.push(filter);
                    }
                    return answer(true);
                }
            }
        }
        if let Some(request) = asked::<f::messages::UpdateDialogFiltersOrder>(body) {
            self.log(&format!(
                "UpdateDialogFiltersOrder {}",
                request.order.iter().map(i32::to_string).collect::<Vec<_>>().join(",")
            ));
            if self.custom_folders.is_none() {
                let defaults = vec![self.folder(WORK), self.folder(FAMILY)];
                self.custom_folders = Some(defaults);
            }
            let folders = self.custom_folders.as_mut().unwrap();
            let id_of = |f: &tl::enums::DialogFilter| match f {
                tl::enums::DialogFilter::Filter(f) => f.id,
                tl::enums::DialogFilter::Chatlist(c) => c.id,
                tl::enums::DialogFilter::Default => 0,
            };
            folders.sort_by_key(|f| request.order.iter().position(|id| *id == id_of(f)).unwrap_or(usize::MAX));
            return answer(true);
        }
        if asked::<f::messages::GetSuggestedDialogFilters>(body).is_some() {
            self.log("GetSuggestedDialogFilters");
            let make = |id: i32, name: &str, contacts: bool, exclude_read: bool, desc: &str| {
                let title = tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities {
                    text: name.to_owned(),
                    entities: Vec::new(),
                });
                let mut filter: tl::types::DialogFilter = blank(&[
                    &put(0u32),
                    &put(id),
                    &put(title),
                    &put(Vec::<tl::enums::InputPeer>::new()),
                    &put(Vec::<tl::enums::InputPeer>::new()),
                    &put(Vec::<tl::enums::InputPeer>::new()),
                ]);
                filter.contacts = contacts;
                filter.non_contacts = !contacts;
                filter.groups = exclude_read;
                filter.exclude_read = exclude_read;
                tl::enums::DialogFilterSuggested::Suggested(tl::types::DialogFilterSuggested {
                    filter: filter.into(),
                    description: desc.to_owned(),
                })
            };
            return answer(vec![
                make(100, "Unread", false, true, "Only unread chats."),
                make(101, "Personal", true, false, "Only private chats with contacts."),
            ]);
        }
        if let Some(request) = asked::<f::messages::GetPinnedDialogs>(body) {
            self.log(&format!("GetPinnedDialogs {}", request.folder_id));
            let (read, unread) = self.crew();
            let dialogs = vec![dialog(Who::Group(CREW), 20, read, 20, unread, true, None), dialog(Who::User(ODALYS), 12, 11, 12, 1, true, None)]
                .into_iter()
                .map(|mut dialog| {
                    let who = Who::of(&dialog.peer());
                    drafted(self, &mut dialog, who);
                    dialog
                })
                .collect();
            let state = tl::types::updates::State { pts: 1, qts: 0, date: t0(), seq: 1, unread_count: 0 };
            return answer(tl::enums::messages::PeerDialogs::Dialogs(tl::types::messages::PeerDialogs { dialogs, messages: Vec::new(), chats: chats(), users: users(), state: state.into() }));
        }
        if let Some(request) = asked::<f::messages::GetPeerDialogs>(body) {
            let peers: Vec<Who> = request.peers.iter().filter_map(|dialog| match dialog {
                tl::enums::InputDialogPeer::Peer(dialog) => Some(input_who(&dialog.peer)),
                _ => None,
            }).collect();
            if peers.contains(&Who::Channel(OFF_PAGE_GROUP)) && std::mem::take(&mut self.off_page_flood) {
                self.log("GetPeerDialogs c9006 flood");
                return Err(rpc(420, "FLOOD_WAIT", Some(1)));
            }
            if peers.contains(&Who::Channel(OFF_PAGE_GROUP)) && std::mem::take(&mut self.off_page_burst) {
                for event in self.steer("off-page-group 102") {
                    let _ = events.send(event);
                }
            }
            let (read, unread) = self.crew();
            let found = peers.iter().filter_map(|who| {
                let dialog = match who {
                    Who::Group(CREW) => dialog(*who, 20, read, 20, unread, false, None),
                    Who::User(ODALYS) => dialog(*who, 12, 11, 12, 1, false, None),
                    Who::User(TOBIN) => dialog(*who, 31, 31, 31, 0, false, None),
                    Who::Channel(NOTES) => dialog(*who, 5, 0, 5, 1, false, None),
                    Who::Channel(OFF_PAGE_GROUP) => self.off_page_dialog(),
                    Who::Channel(CHATTERS) if !self.discussion_left => {
                        let top = self.messages.get(&who.text()).and_then(|messages| messages.last_key_value()).map_or(ECHO, |(id, _)| *id);
                        dialog(*who, top, ECHO, ECHO, i32::from(top > ECHO), false, None)
                    }
                    _ => return None,
                };
                Some((who.clone().text(), dialog))
            }).collect::<Vec<_>>();
            self.log(&format!("GetPeerDialogs {}", found.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>().join(" ")));
            if !found.is_empty() {
                let mut chats = chats();
                if peers.contains(&Who::Channel(OFF_PAGE_GROUP)) {
                    chats.push(off_page_group().into());
                }
                if peers.contains(&Who::Channel(CHATTERS)) {
                    chats.push(discussion_group(self.discussion_left).into());
                }
                let dialogs = found.into_iter().map(|(_, mut dialog)| {
                    let who = Who::of(&dialog.peer());
                    drafted(self, &mut dialog, who);
                    self.flag_unread(&mut dialog);
                    dialog
                }).collect();
                let state = tl::types::updates::State { pts: 1, qts: 0, date: t0(), seq: 1, unread_count: 0 };
                return answer(tl::enums::messages::PeerDialogs::Dialogs(tl::types::messages::PeerDialogs { dialogs, messages: Vec::new(), chats, users: users(), state: state.into() }));
            }
            return Err(rpc(400, "PEER_ID_INVALID", None));
        }
        if let Some(request) = asked::<f::channels::GetChannels>(body) {
            let ids: Vec<_> = request.id.into_iter().filter_map(|input| match input {
                tl::enums::InputChannel::Channel(channel) => Some(channel.channel_id),
                _ => None,
            }).collect();
            self.log(&format!("GetChannels {ids:?}"));
            if ids.contains(&LEFT_BROADCAST) && held_at_wire("TINYCLIENT_TELEGRAM_MEMBERSHIP_FAULT") && !self.membership_fault_sent {
                self.membership_fault_sent = true;
                return Err(rpc(420, "FLOOD_WAIT", Some(1)));
            }
            if ids.contains(&LEFT_BROADCAST) {
                return Err(rpc(406, "CHANNEL_PRIVATE", None));
            }
            if ids.contains(&INVALID_CHANNEL) {
                return Err(rpc(400, "CHANNEL_INVALID", None));
            }
            let chats = chats().into_iter()
                .chain([discussion_group(self.discussion_left).into(), off_page_group().into()])
                .chain(self.created_chats.iter().cloned())
                .filter(|chat| ids.contains(&chat.id())).collect();
            return answer(tl::enums::messages::Chats::Chats(tl::types::messages::Chats { chats }));
        }
        if let Some(request) = asked::<f::contacts::GetContacts>(body) {
            // As Telegram does: the list unchanged since the hash it was asked with is not sent.
            let unchanged = request.hash != 0 && request.hash == super::sync::contacts_hash(1, &[ODALYS]);
            self.log(&format!("GetContacts {}", if unchanged { "unchanged" } else { "listed" }));
            if unchanged {
                return answer(tl::enums::contacts::Contacts::NotModified);
            }
            let contacts = vec![tl::enums::Contact::Contact(tl::types::Contact { user_id: ODALYS, mutual: true })];
            let found = tl::types::contacts::Contacts { contacts, saved_count: 1, users: vec![user(ODALYS, "Odalys", "Brandt", "odalys").into()] };
            return answer(tl::enums::contacts::Contacts::Contacts(found));
        }
        if asked::<f::contacts::GetBlocked>(body).is_some() {
            self.log(&format!("contacts.getBlocked {}", self.blocked.len()));
            let blocked: Vec<tl::enums::PeerBlocked> = self.blocked.iter().map(|id| {
                tl::enums::PeerBlocked::Blocked(tl::types::PeerBlocked { peer_id: Who::User(*id).peer(), date: t0() })
            }).collect();
            let users = self.blocked.iter().map(|id| user(*id, "Blocked", "", "").into()).collect();
            let found = tl::types::contacts::Blocked { blocked, chats: Vec::new(), users };
            return answer(tl::enums::contacts::Blocked::Blocked(found));
        }
        if let Some(request) = asked::<f::contacts::Block>(body) {
            let who = input_who(&request.id);
            if let Who::User(id) = who {
                self.blocked.insert(id);
            }
            self.log(&format!("contacts.block {}", who.text()));
            return answer(true);
        }
        if let Some(request) = asked::<f::contacts::Unblock>(body) {
            let who = input_who(&request.id);
            if let Who::User(id) = who {
                self.blocked.remove(&id);
            }
            self.log(&format!("contacts.unblock {}", who.text()));
            return answer(true);
        }
        if let Some(request) = asked::<f::contacts::AddContact>(body) {
            let id = match &request.id {
                tl::enums::InputUser::User(user) => user.user_id,
                _ => 0,
            };
            self.log(&format!("contacts.addContact u{id} {} {}", request.first_name, request.last_name));
            let mut one = user(id, &request.first_name, &request.last_name, "");
            one.contact = true;
            let made = tl::types::Updates { updates: Vec::new(), users: vec![one.into()], chats: Vec::new(), date: t0(), seq: 0 };
            return answer(tl::enums::Updates::Updates(made));
        }
        if let Some(request) = asked::<f::contacts::DeleteContacts>(body) {
            let ids: Vec<String> = request.id.iter().map(|user| match user {
                tl::enums::InputUser::User(user) => format!("u{}", user.user_id),
                _ => "u0".to_owned(),
            }).collect();
            self.log(&format!("contacts.deleteContacts {}", ids.join(" ")));
            let made = tl::types::Updates { updates: Vec::new(), users: Vec::new(), chats: Vec::new(), date: t0(), seq: 0 };
            return answer(tl::enums::Updates::Updates(made));
        }
        if let Some(request) = asked::<f::contacts::Search>(body) {
            self.log(&format!("contacts.Search {}", request.q));
            let chats = searched_public(&request.q);
            let mut results = vec![Who::User(7_010).peer()];
            results.extend(chats.iter().filter_map(|chat| match chat {
                tl::enums::Chat::Channel(channel) => Some(Who::Channel(channel.id).peer()),
                _ => None,
            }));
            let found = tl::types::contacts::Found {
                my_results: Vec::new(),
                results,
                chats,
                users: vec![user(7_010, "Wren", "Halloran", "wren").into()],
            };
            return answer(tl::enums::contacts::Found::Found(found));
        }
        if let Some(request) = asked::<f::messages::GetHistory>(body) {
            let peer = input_who(&request.peer);
            if request.offset_date > 0 && request.offset_id == 0 {
                self.log(&format!("GetHistory {} date={}", peer.text(), request.offset_date));
                let target = request.offset_date;
                let picked = self.messages.get(&peer.text()).and_then(|held| {
                    held.values()
                        .min_by_key(|m| {
                            let d = match m {
                                tl::enums::Message::Message(m) => m.date,
                                tl::enums::Message::Service(m) => m.date,
                                tl::enums::Message::Empty(_) => 0,
                            };
                            (d - target).abs()
                        })
                        .cloned()
                });
                let messages: Vec<tl::enums::Message> = picked.into_iter().collect();
                return answer(tl::enums::messages::Messages::Messages(tl::types::messages::Messages {
                    messages,
                    topics: Vec::new(),
                    chats: chats(),
                    users: users(),
                }));
            }
            self.log(&format!("GetHistory {} {} {}", peer.text(), request.offset_id, request.add_offset));
            return answer(self.page(&peer.text(), request.offset_id, request.add_offset, request.limit));
        }
        if let Some(request) = asked::<f::messages::GetReplies>(body) {
            let place = key(input_who(&request.peer), Some(request.msg_id));
            self.log(&format!("GetReplies {place}"));
            return answer(self.page(&place, request.offset_id, request.add_offset, request.limit));
        }
        if let Some(request) = asked::<f::messages::GetDiscussionMessage>(body) {
            let place = key(input_who(&request.peer), Some(request.msg_id));
            self.log(&format!("GetDiscussionMessage {place}"));
            return discussion_answer(&place, self.comments_unread, self.discussion_left);
        }
        // A channel's posts and how many saw each: Telegram answers one entry for every id it
        // was asked, in the order it was asked, and the post here has its count.
        if let Some(request) = asked::<f::messages::GetMessagesViews>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("GetMessagesViews {} {}", peer.text(), request.id.iter().map(i32::to_string).collect::<Vec<_>>().join(" ")));
            let views: Vec<tl::enums::MessageViews> = request.id.iter().map(|id| {
                let seen = (*id == 5 && peer == Who::Channel(NOTES)).then_some(12_400);
                tl::enums::MessageViews::Views(tl::types::MessageViews { views: seen, forwards: None, replies: None })
            }).collect();
            let found = tl::types::messages::MessageViews { views, chats: chats(), users: users() };
            return answer(tl::enums::messages::MessageViews::Views(found));
        }
        if let Some(request) = asked::<f::messages::GetCustomEmojiDocuments>(body) {
            // A topic's icon picture: the documents the emoji ids name, stills at list size.
            // The animated icon's own id (5002) is what Telegram names an animated emoji:
            // no still thumbnail, and the file itself is its Lottie play.
            let documents: Vec<tl::enums::Document> = request.document_id.iter().map(|&id| {
                // Installed and recent custom emoji carry the alt the picker shows.
                if id == 6_402 || id == 6_403 {
                    let alt = if id == 6_402 { "\u{2B50}" } else { "\u{2605}" };
                    return custom_emoji(id, alt).into();
                }
                let animated = id == 5002;
                tl::enums::Document::Document(tl::types::Document {
                    id,
                    access_hash: i64::from(id) + 7,
                    file_reference: vec![1u8, 2u8, 3u8],
                    date: t0() - 90,
                    mime_type: if animated { "application/x-tgsticker".to_owned() } else { "image/webp".to_owned() },
                    size: if animated { 20_000 } else { 4_096 + 9 },
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: Vec::new(),
                })
            }).collect();
            self.log(&format!(
                "GetCustomEmojiDocuments {}",
                request.document_id.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
            ));
            return answer(documents);
        }
        if let Some(request) = asked::<f::messages::GetForumTopics>(body) {
            let forum = input_who(&request.peer);
            if request.q.is_none() && request.offset_topic == 0 {
                self.log(&format!("GetForumTopics {}", forum.text()));
            } else {
                self.log(&format!(
                    "GetForumTopics {} offset={} q={:?}",
                    forum.text(),
                    request.offset_topic,
                    request.q
                ));
            }
            return topics_answer(self, forum, &request);
        }
        if let Some(request) = asked::<f::messages::CreateForumTopic>(body) {
            let forum = input_who(&request.peer);
            self.log(&format!("CreateForumTopic {} {}", forum.text(), request.title));
            self.next += 1;
            let id = self.next;
            let now = crate::hub::now() as i32;
            let ft = FakeTopic {
                id,
                title: request.title.clone(),
                date: now,
                top_message: id,
                unread: 0,
                icon_color: request.icon_color.unwrap_or(0x6FB9F0),
                icon_emoji_id: request.icon_emoji_id,
                closed: false,
                pinned: false,
                hidden: false,
                my: true,
            };
            self.forum_topics.entry(forum).or_default().push(ft);
            let service = tl::types::MessageService {
                out: true,
                mentioned: false,
                media_unread: false,
                reactions_are_possible: false,
                silent: false,
                post: false,
                legacy: false,
                id,
                from_id: Some(Who::User(ME).peer()),
                peer_id: forum.peer(),
                saved_peer_id: None,
                reply_to: None,
                date: now,
                action: tl::types::MessageActionTopicCreate {
                    title_missing: false,
                    title: request.title,
                    icon_color: request.icon_color.unwrap_or(0x6FB9F0),
                    icon_emoji_id: request.icon_emoji_id,
                }
                .into(),
                reactions: None,
                ttl_period: None,
            };
            let upd: tl::enums::Update =
                tl::types::UpdateNewChannelMessage { message: service.into(), pts: 0, pts_count: 0 }.into();
            return answer(updates(vec![
                tl::types::UpdateMessageId { id, random_id: request.random_id }.into(),
                upd,
            ]));
        }
        if let Some(request) = asked::<f::messages::EditForumTopic>(body) {
            let forum = input_who(&request.peer);
            self.log(&format!(
                "EditForumTopic {} {} title={:?} closed={:?} hidden={:?}",
                forum.text(),
                request.topic_id,
                request.title,
                request.closed,
                request.hidden
            ));
            if let Some(list) = self.forum_topics.get_mut(&forum) {
                if let Some(ft) = list.iter_mut().find(|t| t.id == request.topic_id) {
                    if let Some(ref title) = request.title {
                        ft.title = title.clone();
                    }
                    if let Some(icon) = request.icon_emoji_id {
                        ft.icon_emoji_id = (icon != 0).then_some(icon);
                    }
                    if let Some(closed) = request.closed {
                        ft.closed = closed;
                    }
                    if let Some(hidden) = request.hidden {
                        ft.hidden = hidden;
                    }
                }
            }
            return answer(updates(Vec::new()));
        }
        if let Some(request) = asked::<f::messages::UpdatePinnedForumTopic>(body) {
            let forum = input_who(&request.peer);
            self.log(&format!("UpdatePinnedForumTopic {} {} {}", forum.text(), request.topic_id, request.pinned));
            if let Some(list) = self.forum_topics.get_mut(&forum) {
                if let Some(ft) = list.iter_mut().find(|t| t.id == request.topic_id) {
                    ft.pinned = request.pinned;
                }
            }
            let upd: tl::enums::Update = tl::types::UpdatePinnedForumTopic {
                pinned: request.pinned,
                peer: forum.peer(),
                topic_id: request.topic_id,
            }
            .into();
            return answer(updates(vec![upd]));
        }
        if let Some(request) = asked::<f::messages::DeleteTopicHistory>(body) {
            let forum = input_who(&request.peer);
            self.log(&format!("DeleteTopicHistory {} {}", forum.text(), request.top_msg_id));
            if let Some(list) = self.forum_topics.get_mut(&forum) {
                list.retain(|t| t.id != request.top_msg_id);
            }
            self.messages.remove(&key(forum, Some(request.top_msg_id)));
            return answer(tl::enums::messages::AffectedHistory::History(
                tl::types::messages::AffectedHistory { pts: 1, pts_count: 1, offset: 0 },
            ));
        }
        if let Some(request) = asked::<f::messages::GetSponsoredMessages>(body) {
            self.log(&format!("GetSponsoredMessages {}", input_who(&request.peer).text()));
            let ad = tl::types::SponsoredMessage {
                recommended: false,
                can_report: true,
                random_id: vec![0xad, 0x01],
                url: "https://example.org/boots".to_owned(),
                title: "Larch Boots".to_owned(),
                message: "Resoled by hand in Innsbruck.".to_owned(),
                entities: None,
                photo: None,
                media: None,
                color: None,
                button_text: "Open site".to_owned(),
                sponsor_info: Some("Larch Boots GmbH".to_owned()),
                additional_info: None,
                min_display_duration: None,
                max_display_duration: None,
            };
            let found = tl::types::messages::SponsoredMessages {
                posts_between: None,
                start_delay: None,
                between_delay: None,
                messages: vec![tl::enums::SponsoredMessage::Message(ad)],
                chats: Vec::new(),
                users: Vec::new(),
            };
            return answer(tl::enums::messages::SponsoredMessages::Messages(found));
        }
        if asked::<f::messages::ViewSponsoredMessage>(body).is_some() {
            self.log("ViewSponsoredMessage");
            return answer(true);
        }
        if asked::<f::messages::ClickSponsoredMessage>(body).is_some() {
            self.log("ClickSponsoredMessage");
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::SendMessage>(body) {
            let peer = input_who(&request.peer);
            if let Some(seconds) = self.flood_send.take() {
                self.log(&format!("SendMessage {} refused {seconds}", peer.text()));
                return Err(rpc(420, "FLOOD_WAIT", Some(seconds)));
            }
            if request.message == "refuse me" {
                return Err(rpc(403, "CHAT_WRITE_FORBIDDEN", None));
            }
            if request.message == "premium file" {
                return Err(rpc(403, "PREMIUM_ACCOUNT_REQUIRED", None));
            }
            let peer_id = match peer {
                Who::User(id) | Who::Channel(id) | Who::Group(id) => id,
            };
            if let Some(&price) = self.paid_stars.get(&peer_id).filter(|&&s| s > 0) {
                if request.allow_paid_stars.unwrap_or(0) < price {
                    return Err(rpc(403, "ALLOW_PAYMENT_REQUIRED", Some(price as u32)));
                }
            }
            let (topic, reply) = match &request.reply_to {
                Some(tl::enums::InputReplyTo::Message(reply)) if peer == Who::Channel(WORKSHOP) => (Some(reply.top_msg_id.unwrap_or(reply.reply_to_msg_id)), None),
                Some(tl::enums::InputReplyTo::Message(reply)) => (None, Some(reply.reply_to_msg_id)),
                _ => (None, None),
            };
            let options = [("silent", request.silent), ("no-preview", request.no_webpage), ("scheduled", request.schedule_date.is_some())];
            let mut options: String = options.iter().filter(|(_, on)| *on).map(|(name, _)| format!(" [{name}]")).collect();
            if let Some(stars) = request.allow_paid_stars {
                options.push_str(&format!(" [stars={stars}]"));
            }
            // A comment names the echo post it answers in its key, as a forum's send names
            // its topic: the two are Telegram's own ways to point at a threaded write.
            let said = match peer == Who::Channel(CHATTERS) {
                true => key(peer, reply),
                false => key(peer, topic),
            };
            let noted = super::content::entity_note(request.entities.as_deref().unwrap_or(&[]));
            self.log(&format!("SendMessage {said} {}{noted}{options}", request.message));
            return self.deliver(events, peer, topic, reply, vec![(request.random_id, request.message, None, request.entities)], request.schedule_date);
        }
        if let Some(request) = asked::<f::messages::EditMessage>(body) {
            let peer = input_who(&request.peer);
            let noted = super::content::entity_note(request.entities.as_deref().unwrap_or(&[]));
            let replaced = request.media.as_ref().map(|m| held_media(m, self.next + 4_000));
            let media_note = replaced.as_ref().map(|(kind, _)| format!(" media={kind}")).unwrap_or_default();
            let invert_note = if request.invert_media { " [invert]" } else { "" };
            self.log(&format!("EditMessage {} {}{noted}{media_note}{invert_note}", peer.text(), request.id));
            if let Some(tl::enums::Message::Message(mut sched)) = self
                .scheduled
                .get(&peer.text())
                .and_then(|held| held.get(&request.id))
                .cloned()
            {
                if let Some(ref text) = request.message {
                    sched.message = text.clone();
                }
                if let Some(date) = request.schedule_date {
                    sched.date = date;
                }
                sched.entities = request.entities.clone();
                self.scheduled
                    .entry(peer.text())
                    .or_default()
                    .insert(request.id, sched.clone().into());
                return answer(updates(vec![tl::types::UpdateNewScheduledMessage { message: sched.into() }.into()]));
            }
            let Some(tl::enums::Message::Message(mut edited)) = self.messages.get(&peer.text()).and_then(|held| held.get(&request.id)).cloned() else {
                return Err(rpc(400, "MESSAGE_ID_INVALID", None));
            };
            if !edited.out && !edited.post {
                return Err(rpc(403, "MESSAGE_AUTHOR_REQUIRED", None));
            }
            if let Some(text) = request.message.clone() {
                edited.message = text;
            }
            edited.entities = request.entities.clone();
            if let Some((_, media)) = replaced {
                edited.media = Some(media);
            }
            edited.invert_media = request.invert_media;
            edited.edit_date = Some(crate::hub::now() as i32);
            self.store(None, edited.clone());
            let upd: tl::enums::Update = match peer {
                Who::Channel(_) => tl::types::UpdateEditChannelMessage { message: edited.into(), pts: 0, pts_count: 0 }.into(),
                _ => tl::types::UpdateEditMessage { message: edited.into(), pts: 0, pts_count: 0 }.into(),
            };
            return answer(updates(vec![upd]));
        }
        if let Some(request) = asked::<f::messages::GetScheduledHistory>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("GetScheduledHistory {}", peer.text()));
            let prefix = format!("{}_", peer.text());
            let messages: Vec<tl::enums::Message> = self
                .scheduled
                .iter()
                .filter(|(k, _)| k.as_str() == peer.text() || k.starts_with(&prefix))
                .flat_map(|(_, held)| held.values().cloned())
                .collect();
            return answer(tl::enums::messages::Messages::Messages(tl::types::messages::Messages {
                messages,
                topics: Vec::new(),
                chats: chats(),
                users: users(),
            }));
        }
        if let Some(request) = asked::<f::messages::SendScheduledMessages>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("SendScheduledMessages {} {:?}", peer.text(), request.id));
            let mut upds: Vec<tl::enums::Update> = Vec::new();
            let mut sent_ids: Vec<i32> = Vec::new();
            let prefix = format!("{}_", peer.text());
            for (k, held) in self.scheduled.iter_mut() {
                if k.as_str() != peer.text() && !k.starts_with(&prefix) {
                    continue;
                }
                for &id in &request.id {
                    if let Some(tl::enums::Message::Message(mut msg)) = held.remove(&id) {
                        msg.from_scheduled = true;
                        msg.date = crate::hub::now() as i32;
                        let topic = msg.reply_to.as_ref().and_then(|r| match r {
                            tl::enums::MessageReplyHeader::Header(h) if h.forum_topic => h.reply_to_top_id.or(h.reply_to_msg_id),
                            _ => None,
                        });
                        self.messages.entry(key(peer, topic)).or_default().insert(msg.id, msg.clone().into());
                        sent_ids.push(id);
                        let new: tl::enums::Update = match peer {
                            Who::Channel(_) => tl::types::UpdateNewChannelMessage { message: msg.into(), pts: 0, pts_count: 0 }.into(),
                            _ => tl::types::UpdateNewMessage { message: msg.into(), pts: 0, pts_count: 0 }.into(),
                        };
                        let _ = events.send(update(new.clone()));
                        upds.push(new);
                    }
                }
            }
            if !sent_ids.is_empty() {
                upds.push(tl::types::UpdateDeleteScheduledMessages {
                    peer: peer.peer(),
                    messages: sent_ids,
                    sent_messages: None,
                }.into());
            }
            return answer(updates(upds));
        }
        if let Some(request) = asked::<f::messages::DeleteScheduledMessages>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("DeleteScheduledMessages {} {:?}", peer.text(), request.id));
            let prefix = format!("{}_", peer.text());
            for (k, held) in self.scheduled.iter_mut() {
                if k.as_str() != peer.text() && !k.starts_with(&prefix) {
                    continue;
                }
                for &id in &request.id {
                    held.remove(&id);
                }
            }
            return answer(updates(vec![tl::types::UpdateDeleteScheduledMessages {
                peer: peer.peer(),
                messages: request.id,
                sent_messages: None,
            }.into()]));
        }
        if let Some(request) = asked::<f::messages::DeleteMessages>(body) {
            self.log(&format!("DeleteMessages {:?} revoke {}", request.id, request.revoke));
            if request.revoke && request.id.contains(&20) && std::env::var("TINYCLIENT_TELEGRAM_OLD_GROUP").ok().as_deref() == Some("1") {
                return Err(rpc(403, "MESSAGE_DELETE_FORBIDDEN", None));
            }
            for held in self.messages.values_mut() {
                request.id.iter().for_each(|id| drop(held.remove(id)));
            }
            return answer(tl::enums::messages::AffectedMessages::Messages(tl::types::messages::AffectedMessages { pts: 2, pts_count: 1 }));
        }
        if let Some(request) = asked::<f::channels::DeleteMessages>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Err(rpc(400, "CHANNEL_INVALID", None));
            };
            self.log(&format!("channels.DeleteMessages c{} {:?}", channel.channel_id, request.id));
            let is_admin = std::env::var("TINYCLIENT_TELEGRAM_ADMIN").ok().as_deref() == Some("1");
            let prefix = format!("c{}", channel.channel_id);
            for (place, held) in self.messages.iter_mut() {
                if place == &prefix || place.starts_with(&format!("{prefix}_")) {
                    for id in &request.id {
                        if let Some(tl::enums::Message::Message(msg)) = held.get(id) {
                            if !msg.out && !is_admin {
                                return Err(rpc(403, "MESSAGE_DELETE_FORBIDDEN", None));
                            }
                        }
                        held.remove(id);
                    }
                }
            }
            return answer(tl::enums::messages::AffectedMessages::Messages(tl::types::messages::AffectedMessages { pts: 2, pts_count: 1 }));
        }
        if let Some(request) = asked::<f::channels::DeleteParticipantHistory>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Err(rpc(400, "CHANNEL_INVALID", None));
            };
            let who = input_who(&request.participant);
            self.log(&format!("channels.DeleteParticipantHistory c{} {}", channel.channel_id, who.text()));
            let prefix = format!("c{}", channel.channel_id);
            for (place, held) in self.messages.iter_mut() {
                if place == &prefix || place.starts_with(&format!("{prefix}_")) {
                    held.retain(|_, msg| match msg {
                        tl::enums::Message::Message(m) => m.from_id.as_ref().is_none_or(|p| Who::of(p) != who),
                        _ => true,
                    });
                }
            }
            return answer(tl::enums::messages::AffectedHistory::History(
                tl::types::messages::AffectedHistory { pts: 2, pts_count: 1, offset: 0 },
            ));
        }
        if let Some(request) = asked::<f::channels::ReportSpam>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Err(rpc(400, "CHANNEL_INVALID", None));
            };
            let who = input_who(&request.participant);
            self.log(&format!("channels.ReportSpam c{} {} {:?}", channel.channel_id, who.text(), request.id));
            return answer(true);
        }
        if asked::<f::messages::GetAvailableReactions>(body).is_some() {
            self.log("GetAvailableReactions");
            // Telegram's list, short: 🚫 is one Telegram then refuses, and 🦄 needs Premium.
            let one = |emoji: &str, premium: bool| {
                let mut reaction: tl::types::AvailableReaction = blank(&[
                    &put(0u32),
                    &put(emoji.to_owned()),
                    &put(emoji.to_owned()),
                    &put(tl::enums::Document::Empty(tl::types::DocumentEmpty { id: 0 })),
                    &put(tl::enums::Document::Empty(tl::types::DocumentEmpty { id: 0 })),
                    &put(tl::enums::Document::Empty(tl::types::DocumentEmpty { id: 0 })),
                    &put(tl::enums::Document::Empty(tl::types::DocumentEmpty { id: 0 })),
                    &put(tl::enums::Document::Empty(tl::types::DocumentEmpty { id: 0 })),
                ]);
                reaction.premium = premium;
                tl::enums::AvailableReaction::Reaction(reaction)
            };
            let reactions = vec![one("👍", false), one("❤", false), one("🔥", false), one("🎉", false), one("🚫", false), one("🦄", true)];
            return answer(tl::enums::messages::AvailableReactions::Reactions(tl::types::messages::AvailableReactions { hash: 1, reactions }));
        }
        if let Some(request) = asked::<f::messages::SendReaction>(body) {
            let peer = input_who(&request.peer);
            let sent: Vec<String> = request.reaction.iter().flatten().filter_map(|reaction| match reaction {
                tl::enums::Reaction::Emoji(emoji) => Some(emoji.emoticon.clone()),
                _ => None,
            }).collect();
            if request.big {
                self.log(&format!("SendReaction {} {} {} big", peer.text(), request.msg_id, sent.join("")));
            } else {
                self.log(&format!("SendReaction {} {} {}", peer.text(), request.msg_id, sent.join("")));
            }
            if sent.iter().any(|emoji| emoji == "🚫") {
                return Err(rpc(400, "REACTION_INVALID", None));
            }
            let results = sent.iter().map(|emoji| {
                tl::enums::ReactionCount::Count(tl::types::ReactionCount { chosen_order: Some(0), reaction: tl::types::ReactionEmoji { emoticon: emoji.clone() }.into(), count: 1 })
            });
            let reactions = tl::types::MessageReactions { min: false, can_see_list: true, reactions_as_tags: false, results: results.collect(), recent_reactions: None, top_reactors: None };
            let reacted = tl::types::UpdateMessageReactions { peer: peer.peer(), msg_id: request.msg_id, top_msg_id: None, saved_peer_id: None, reactions: reactions.into() };
            return answer(updates(vec![reacted.into()]));
        }
        if let Some(request) = asked::<f::messages::SendPaidReaction>(body) {
            let peer = input_who(&request.peer);
            let privacy = match &request.private {
                Some(tl::enums::PaidReactionPrivacy::Anonymous) => "anonymous",
                _ => "default",
            };
            self.log(&format!("SendPaidReaction {} {} count={} private={privacy}", peer.text(), request.msg_id, request.count));
            if i64::from(request.count) > self.stars_balance {
                return Err(rpc(400, "BALANCE_TOO_LOW", None));
            }
            self.stars_balance -= i64::from(request.count);
            let entry = self.paid_counts.entry((peer, request.msg_id)).or_insert(0);
            *entry += request.count;
            let total = *entry;
            let anonymous = privacy == "anonymous";
            let top_reactors = vec![
                tl::enums::MessageReactor::Reactor(tl::types::MessageReactor {
                    top: true,
                    my: true,
                    anonymous,
                    peer_id: (!anonymous).then(|| Who::User(ME).peer()),
                    count: request.count,
                }),
                tl::enums::MessageReactor::Reactor(tl::types::MessageReactor {
                    top: true,
                    my: false,
                    anonymous: false,
                    peer_id: Some(Who::User(ODALYS).peer()),
                    count: 5,
                }),
            ];
            let reactions = tl::types::MessageReactions {
                min: false,
                can_see_list: true,
                reactions_as_tags: false,
                results: vec![tl::enums::ReactionCount::Count(tl::types::ReactionCount {
                    chosen_order: Some(0),
                    reaction: tl::enums::Reaction::Paid,
                    count: total,
                })],
                recent_reactions: None,
                top_reactors: Some(top_reactors),
            };
            let top_msg_id = (peer == Who::Channel(WORKSHOP) && request.msg_id == 41).then_some(TOOLS);
            let reacted = tl::types::UpdateMessageReactions {
                peer: peer.peer(),
                msg_id: request.msg_id,
                top_msg_id,
                saved_peer_id: None,
                reactions: reactions.into(),
            };
            return answer(updates(vec![reacted.into()]));
        }
        if let Some(request) = asked::<f::messages::TogglePaidReactionPrivacy>(body) {
            let peer = input_who(&request.peer);
            let privacy = match &request.private {
                tl::enums::PaidReactionPrivacy::Anonymous => "anonymous",
                _ => "default",
            };
            self.log(&format!("TogglePaidReactionPrivacy {} {} private={privacy}", peer.text(), request.msg_id));
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::SetDefaultReaction>(body) {
            let emoji = match &request.reaction {
                tl::enums::Reaction::Emoji(e) => e.emoticon.clone(),
                tl::enums::Reaction::CustomEmoji(c) => format!("e{}", c.document_id),
                _ => String::new(),
            };
            self.default_reaction = emoji.clone();
            self.log(&format!("SetDefaultReaction {emoji}"));
            return answer(true);
        }
        if asked::<f::payments::GetStarsStatus>(body).is_some() {
            self.log("GetStarsStatus");
            let status = tl::types::payments::StarsStatus {
                balance: tl::enums::StarsAmount::Amount(tl::types::StarsAmount { amount: self.stars_balance, nanos: 0 }),
                subscriptions: None,
                subscriptions_next_offset: None,
                subscriptions_missing_balance: None,
                history: None,
                next_offset: None,
                chats: Vec::new(),
                users: Vec::new(),
            };
            return answer(tl::enums::payments::StarsStatus::Status(status));
        }
        if let Some(request) = asked::<f::messages::MarkDialogUnread>(body) {
            let who = match &request.peer {
                tl::enums::InputDialogPeer::Peer(dialog) => input_who(&dialog.peer),
                _ => return Err(rpc(400, "PEER_ID_INVALID", None)),
            };
            self.unread_marks.insert(who, request.unread);
            self.log(&format!("MarkDialogUnread {} {}", who.text(), if request.unread { "unread" } else { "read" }));
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::ReadHistory>(body) {
            let who = input_who(&request.peer);
            self.log(&format!("ReadHistory {} {}", who.text(), request.max_id));
            // A read of the group that reaches this fake is heard: what was lost of it is told again.
            if who == Who::Group(CREW) {
                self.lost_reads = false;
            }
            return answer(tl::enums::messages::AffectedMessages::Messages(tl::types::messages::AffectedMessages { pts: 3, pts_count: 1 }));
        }
        if let Some(request) = asked::<f::channels::ReadHistory>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Err(rpc(400, "CHANNEL_INVALID", None));
            };
            self.log(&format!("channels.ReadHistory c{} {}", channel.channel_id, request.max_id));
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::ReadMessageContents>(body) {
            let ids: Vec<String> = request.id.iter().map(i32::to_string).collect();
            self.log(&format!("ReadMessageContents {}", ids.join(",")));
            return answer(tl::enums::messages::AffectedMessages::Messages(tl::types::messages::AffectedMessages { pts: 4, pts_count: 1 }));
        }
        if let Some(request) = asked::<f::channels::ReadMessageContents>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Err(rpc(400, "CHANNEL_INVALID", None));
            };
            let ids: Vec<String> = request.id.iter().map(i32::to_string).collect();
            self.log(&format!("channels.ReadMessageContents c{} {}", channel.channel_id, ids.join(",")));
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::ReadDiscussion>(body) {
            let forum = input_who(&request.peer);
            self.log(&format!("ReadDiscussion {} {}", key(forum, Some(request.msg_id)), request.read_max_id));
            // The read reaches this fake, as Telegram's would: the topic's list says it read
            // from here on.
            if request.read_max_id > 0 {
                self.topic_reads.insert((forum, request.msg_id), request.read_max_id);
            }
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::SetTyping>(body) {
            self.log(&format!("SetTyping {} {}", input_who(&request.peer).text(), super::live::action_word(&request.action)));
            return answer(true);
        }
        if let Some(request) = asked::<f::upload::GetFile>(body) {
            let what = match &request.location {
                tl::enums::InputFileLocation::InputPhotoFileLocation(photo) => format!("photo {} {}", photo.id, photo.thumb_size),
                tl::enums::InputFileLocation::InputPeerPhotoFileLocation(photo) => format!("avatar {}", photo.photo_id),
                tl::enums::InputFileLocation::InputDocumentFileLocation(document) => format!("document {} {}", document.id, document.thumb_size).trim_end().to_owned(),
                _ => "other".to_owned(),
            };
            if let Some(seconds) = self.flood_file.take() {
                self.log(&format!("GetFile {what} refused {seconds}"));
                return Err(rpc(420, "FLOOD_WAIT", Some(seconds)));
            }
            self.log(&format!("GetFile {what} at {}", request.offset));
            if let tl::enums::InputFileLocation::InputDocumentFileLocation(document) = &request.location {
                if document.id == 9_300 {
                    let redirect = tl::types::upload::FileCdnRedirect {
                        dc_id: 5,
                        file_token: b"cdn-token".to_vec(),
                        encryption_key: CDN_KEY.to_vec(),
                        encryption_iv: CDN_IV.to_vec(),
                        file_hashes: Vec::new(),
                    };
                    self.file_in += 1;
                    self.file_at_once = self.file_at_once.max(self.file_in);
                    let answered = answer(tl::enums::upload::File::CdnRedirect(redirect));
                    self.file_in -= 1;
                    return answered;
                }
                if document.id == 9_400 && request.offset > 0 && !self.large_broke {
                    self.large_broke = true;
                    return Err(Fault::Net("the transfer dropped".into()));
                }
            }
            let held = match &request.location {
                tl::enums::InputFileLocation::InputDocumentFileLocation(document) if document.id == 5001 || document.id == 5002 => {
                    packed(br#"{"tg":"the topic's icon"}"#)
                }
                location if is_ridge_thumb(location) => piece_of(RIDGE_M, request.offset, request.limit),
                location if is_crew_thumb(location) => piece_of(CREW_PHOTO_M, request.offset, request.limit),
                tl::enums::InputFileLocation::InputDocumentFileLocation(document) if document.id == 9_400 => big_piece(request.offset, request.limit),
                _ => file_piece(request.offset, request.limit),
            };
            let piece = tl::types::upload::File { r#type: tl::enums::storage::FileType::FileJpeg, mtime: t0(), bytes: held };
            self.file_in += 1;
            self.file_at_once = self.file_at_once.max(self.file_in);
            let answered = answer(tl::enums::upload::File::File(piece));
            self.file_in -= 1;
            return answered;
        }
        if let Some(request) = asked::<f::upload::GetCdnFile>(body) {
            self.log("GetCdnFile");
            if !self.cdn_reuploaded {
                self.log("CdnReuploadNeeded");
                return answer(tl::enums::upload::CdnFile::ReuploadNeeded(tl::types::upload::CdnFileReuploadNeeded {
                    request_token: b"again".to_vec(),
                }));
            }
            let bytes = cdn_cipher(request.offset, request.limit);
            return answer(tl::enums::upload::CdnFile::File(tl::types::upload::CdnFile { bytes }));
        }
        if asked::<f::upload::ReuploadCdnFile>(body).is_some() {
            self.cdn_reuploaded = true;
            self.log("ReuploadCdnFile");
            return answer(vec![cdn_hash()]);
        }
        if asked::<f::upload::GetCdnFileHashes>(body).is_some() {
            self.log("GetCdnFileHashes");
            return answer(vec![cdn_hash()]);
        }
        if let Some(request) = asked::<f::auth::ExportAuthorization>(body) {
            self.log(&format!("ExportAuthorization {}", request.dc_id));
            return answer(tl::enums::auth::ExportedAuthorization::Authorization(tl::types::auth::ExportedAuthorization { id: ME, bytes: vec![1; 8] }));
        }
        if asked::<f::auth::ImportAuthorization>(body).is_some() {
            self.log("ImportAuthorization");
            return answer(self.me());
        }
        if let Some(request) = asked::<f::messages::GetFullChat>(body) {
            let one = |user_id| tl::enums::ChatParticipant::Participant(tl::types::ChatParticipant { user_id, inviter_id: TOBIN, date: t0(), rank: None });
            let participants = tl::types::ChatParticipants { chat_id: request.chat_id, participants: vec![one(TOBIN), one(ODALYS), one(ME)], version: 1 };
            let mut full: tl::types::ChatFull = blank(&[
                &put(0u32),
                &put(request.chat_id),
                &put(String::new()),
                &put(tl::enums::ChatParticipants::Participants(participants)),
                &put(settings(None)),
            ]);
            full.about = "Weekend routes.".to_owned();
            full.ttl_period = self.ttl_periods.get(&Who::Group(request.chat_id)).copied().filter(|&p| p > 0);
            // Its admins allow three reactions.
            let three = ["👍", "❤", "🔥"].map(|emoticon| tl::types::ReactionEmoji { emoticon: emoticon.to_owned() }.into());
            full.available_reactions = Some(tl::types::ChatReactionsSome { reactions: three.to_vec() }.into());
            let found = tl::types::messages::ChatFull { full_chat: full.into(), chats: chats(), users: users() };
            return answer(tl::enums::messages::ChatFull::Full(found));
        }
        if let Some(request) = asked::<f::users::GetFullUser>(body) {
            let id = match &request.id {
                tl::enums::InputUser::User(user) => user.user_id,
                _ => ME,
            };
            let peer_settings: tl::types::PeerSettings = blank(&[&put(0u32)]);
            let mut full: tl::types::UserFull = blank(&[&put(0u32), &put(0u32), &put(id), &put(tl::enums::PeerSettings::from(peer_settings)), &put(settings(None)), &put(1i32)]);
            full.about = Some("Maps, mostly.".to_owned());
            full.ttl_period = self.ttl_periods.get(&Who::User(id)).copied().filter(|&p| p > 0);
            full.blocked = self.blocked.contains(&id);
            let (phone_avail, phone_priv, video_avail) = self.call_flags.get(&id).copied().unwrap_or((true, false, false));
            full.phone_calls_available = phone_avail;
            full.phone_calls_private = phone_priv;
            full.video_calls_available = video_avail;
            full.send_paid_messages_stars = self.paid_stars.get(&id).copied();
            full.voice_messages_forbidden = self.voice_forbidden.contains(&id);
            if id == BOT {
                full.bot_info = Some(trail_bot_info());
            }
            let mut people = users();
            if id == ME {
                full.about = Some(self.me_about.clone());
                full.birthday = self.me_birthday.clone();
                if let Some(slot) = people.iter_mut().find(|one| matches!(one, tl::enums::User::User(u) if u.id == ME)) {
                    *slot = self.self_user().into();
                }
            }
            if id == TOBIN {
                full.birthday = Some(tl::types::Birthday { day: 14, month: 3, year: None }.into());
                full.business_location = Some(tl::types::BusinessLocation { geo_point: None, address: "The hut".to_owned() }.into());
                full.business_work_hours = Some(tl::types::BusinessWorkHours {
                    open_now: true,
                    timezone_id: "Europe/Lisbon".to_owned(),
                    weekly_open: Vec::new(),
                }.into());
                if let Some(tl::enums::User::User(user)) = people.iter_mut().find(|one| matches!(one, tl::enums::User::User(user) if user.id == TOBIN)) {
                    user.usernames = Some(vec![
                        tl::types::Username { editable: true, active: true, username: "ridge".to_owned() }.into(),
                        tl::types::Username { editable: false, active: true, username: "tobin_r".to_owned() }.into(),
                    ]);
                }
            }
            let found = tl::types::users::UserFull { full_user: full.into(), chats: Vec::new(), users: people };
            return answer(tl::enums::users::UserFull::Full(found));
        }
        if let Some(request) = asked::<f::account::UpdateProfile>(body) {
            if let Some(first) = request.first_name {
                self.me_first = first;
            }
            if let Some(last) = request.last_name {
                self.me_last = last;
            }
            if let Some(about) = request.about {
                self.me_about = about;
            }
            self.log(&format!("UpdateProfile {} {} | {}", self.me_first, self.me_last, self.me_about));
            return answer(tl::enums::User::from(self.self_user()));
        }
        if let Some(request) = asked::<f::account::CheckUsername>(body) {
            self.log(&format!("CheckUsername {}", request.username));
            let ok = !matches!(request.username.to_ascii_lowercase().as_str(), "taken_name" | "odalys");
            return answer(ok);
        }
        if let Some(request) = asked::<f::account::UpdateUsername>(body) {
            self.log(&format!("UpdateUsername {}", request.username));
            self.me_username = request.username;
            return answer(tl::enums::User::from(self.self_user()));
        }
        if let Some(request) = asked::<f::account::UpdateBirthday>(body) {
            self.me_birthday = request.birthday.clone();
            let note = match &request.birthday {
                Some(tl::enums::Birthday::Birthday(b)) => format!("{}-{}-{}", b.year.unwrap_or(0), b.month, b.day),
                None => "clear".to_owned(),
            };
            self.log(&format!("UpdateBirthday {note}"));
            return answer(true);
        }
        if let Some(request) = asked::<f::account::UpdateEmojiStatus>(body) {
            let premium = self.self_user().premium;
            match request.emoji_status {
                tl::enums::EmojiStatus::Empty => {
                    self.me_emoji_status = None;
                    self.log("UpdateEmojiStatus clear");
                    return answer(true);
                }
                tl::enums::EmojiStatus::Status(st) => {
                    if !premium {
                        return Err(rpc(403, "PREMIUM_ACCOUNT_REQUIRED", None));
                    }
                    self.me_emoji_status = Some(st.document_id);
                    self.log(&format!("UpdateEmojiStatus {} until={:?}", st.document_id, st.until));
                    return answer(true);
                }
                _ => return answer(true),
            }
        }
        if asked::<f::photos::UploadProfilePhoto>(body).is_some() {
            self.me_photo_id = Some(6_900);
            self.log("UploadProfilePhoto");
            let saved = tl::types::photos::Photo {
                photo: photo(6_900),
                users: vec![self.self_user().into()],
            };
            return answer(tl::enums::photos::Photo::Photo(saved));
        }
        if asked::<f::photos::GetUserPhotos>(body).is_some() {
            self.log("GetUserPhotos");
            let photos: Vec<tl::enums::Photo> = self.me_photo_id.map(photo).into_iter().collect();
            let listed = tl::types::photos::Photos {
                photos,
                users: vec![self.self_user().into()],
            };
            return answer(tl::enums::photos::Photos::Photos(listed));
        }
        if let Some(request) = asked::<f::photos::DeletePhotos>(body) {
            self.log(&format!("DeletePhotos {}", request.id.len()));
            self.me_photo_id = None;
            let ids: Vec<i64> = request
                .id
                .into_iter()
                .filter_map(|p| match p {
                    tl::enums::InputPhoto::Photo(p) => Some(p.id),
                    _ => None,
                })
                .collect();
            return answer(ids);
        }
        if let Some(request) = asked::<f::messages::CreateChat>(body) {
            self.next_chat_id += 1;
            let chat_id = self.next_chat_id;
            let user_ids: Vec<i64> = request
                .users
                .iter()
                .filter_map(|u| match u {
                    tl::enums::InputUser::User(u) => Some(u.user_id),
                    _ => None,
                })
                .collect();
            self.log(&format!("CreateChat {} users={:?}", request.title, user_ids));
            let chat = tl::types::Chat {
                creator: true,
                left: false,
                deactivated: false,
                call_active: false,
                call_not_empty: false,
                noforwards: false,
                id: chat_id,
                title: request.title.clone(),
                photo: tl::enums::ChatPhoto::Empty,
                participants_count: 1 + user_ids.len() as i32,
                date: crate::hub::now() as i32,
                version: 1,
                migrated_to: None,
                admin_rights: None,
                default_banned_rights: None,
            };
            let chat_enum: tl::enums::Chat = chat.into();
            self.created_chats.push(chat_enum.clone());
            self.next += 1;
            let sys = tl::types::MessageService {
                out: true,
                mentioned: false,
                media_unread: false,
                reactions_are_possible: false,
                silent: false,
                post: false,
                legacy: false,
                id: self.next,
                from_id: Some(Who::User(ME).peer()),
                peer_id: Who::Group(chat_id).peer(),
                saved_peer_id: None,
                reply_to: None,
                date: crate::hub::now() as i32,
                action: tl::types::MessageActionChatCreate { title: request.title, users: user_ids.clone() }.into(),
                reactions: None,
                ttl_period: None,
            };
            let upd = tl::types::Updates {
                updates: vec![tl::types::UpdateNewMessage { message: sys.into(), pts: 0, pts_count: 0 }.into()],
                users: users(),
                chats: vec![chat_enum],
                date: crate::hub::now() as i32,
                seq: 0,
            };
            let missing: Vec<tl::enums::MissingInvitee> = user_ids
                .iter()
                .filter(|&&id| id == SCAM)
                .map(|&user_id| {
                    tl::types::MissingInvitee {
                        premium_would_allow_invite: false,
                        premium_required_for_pm: false,
                        user_id,
                    }
                    .into()
                })
                .collect();
            let result = tl::types::messages::InvitedUsers {
                updates: tl::enums::Updates::Updates(upd),
                missing_invitees: missing,
            };
            return answer(tl::enums::messages::InvitedUsers::Users(result));
        }
        if let Some(request) = asked::<f::channels::CreateChannel>(body) {
            self.next_chat_id += 1;
            let chan_id = self.next_chat_id;
            self.log(&format!(
                "CreateChannel {} broadcast={} megagroup={} about={}",
                request.title, request.broadcast, request.megagroup, request.about
            ));
            let mut ch = a_channel(chan_id, &request.title, request.broadcast, request.forum, "");
            ch.creator = true;
            ch.megagroup = request.megagroup;
            let chat_enum: tl::enums::Chat = tl::enums::Chat::Channel(ch);
            self.created_chats.push(chat_enum.clone());
            let upd = tl::types::Updates {
                updates: Vec::new(),
                users: users(),
                chats: vec![chat_enum],
                date: crate::hub::now() as i32,
                seq: 0,
            };
            return answer(tl::enums::Updates::Updates(upd));
        }
        if let Some(request) = asked::<f::channels::CheckUsername>(body) {
            self.log(&format!("ChannelCheckUsername {}", request.username));
            let ok = !matches!(request.username.to_ascii_lowercase().as_str(), "taken_chan" | "fieldnotes");
            return answer(ok);
        }
        if let Some(request) = asked::<f::channels::UpdateUsername>(body) {
            let chan_id = match request.channel {
                tl::enums::InputChannel::Channel(c) => c.channel_id,
                _ => 0,
            };
            self.log(&format!("ChannelUpdateUsername c{chan_id} {}", request.username));
            for chat in &mut self.created_chats {
                if let tl::enums::Chat::Channel(c) = chat {
                    if c.id == chan_id {
                        c.username = (!request.username.is_empty()).then(|| request.username.clone());
                    }
                }
            }
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::AddChatUser>(body) {
            let uid = match request.user_id {
                tl::enums::InputUser::User(u) => u.user_id,
                _ => 0,
            };
            self.log(&format!("AddChatUser g{} u{uid}", request.chat_id));
            let missing = if uid == SCAM {
                vec![tl::types::MissingInvitee {
                    premium_would_allow_invite: false,
                    premium_required_for_pm: false,
                    user_id: uid,
                }
                .into()]
            } else {
                Vec::new()
            };
            let result = tl::types::messages::InvitedUsers {
                updates: updates(Vec::new()),
                missing_invitees: missing,
            };
            return answer(tl::enums::messages::InvitedUsers::Users(result));
        }
        if let Some(request) = asked::<f::messages::CheckChatInvite>(body) {
            self.log(&format!("CheckChatInvite {}", request.hash));
            match request.hash.as_str() {
                "expired_invite" => return Err(rpc(400, "INVITE_HASH_EXPIRED", None)),
                "already_crew" => {
                    let crew = chats().into_iter().next().unwrap();
                    return answer(tl::enums::ChatInvite::Already(tl::types::ChatInviteAlready { chat: crew }));
                }
                "approval_invite" => {
                    let mut inv: tl::types::ChatInvite = blank(&[
                        &put(0u32),
                        &put("Alpine Guides".to_owned()),
                        &put(tl::enums::Photo::Empty(tl::types::PhotoEmpty { id: 0 })),
                        &put(42i32),
                        &put(0i32),
                    ]);
                    inv.channel = true;
                    inv.megagroup = true;
                    inv.request_needed = true;
                    inv.about = Some("Private mountain guides".to_owned());
                    return answer(tl::enums::ChatInvite::Invite(inv));
                }
                _ => {
                    let mut inv: tl::types::ChatInvite = blank(&[
                        &put(0u32),
                        &put("Alpine Club".to_owned()),
                        &put(tl::enums::Photo::Empty(tl::types::PhotoEmpty { id: 0 })),
                        &put(128i32),
                        &put(0i32),
                    ]);
                    inv.channel = true;
                    inv.broadcast = true;
                    inv.about = Some("Trail conditions and trip reports".to_owned());
                    return answer(tl::enums::ChatInvite::Invite(inv));
                }
            }
        }
        if let Some(request) = asked::<f::messages::ImportChatInvite>(body) {
            self.log(&format!("ImportChatInvite {}", request.hash));
            if request.hash == "expired_invite" {
                return Err(rpc(400, "INVITE_HASH_EXPIRED", None));
            }
            if request.hash == "approval_invite" {
                return Err(rpc(400, "INVITE_REQUEST_SENT", None));
            }
            let chan = a_channel(9_201, "Alpine Club", true, false, "");
            let chat_enum: tl::enums::Chat = tl::enums::Chat::Channel(chan);
            self.created_chats.push(chat_enum.clone());
            let upd = tl::types::Updates {
                updates: Vec::new(),
                users: users(),
                chats: vec![chat_enum],
                date: crate::hub::now() as i32,
                seq: 0,
            };
            return answer(tl::enums::messages::ChatInviteJoinResult::Ok(
                tl::types::messages::ChatInviteJoinResultOk {
                    updates: tl::enums::Updates::Updates(upd),
                },
            ));
        }
        if let Some(request) = asked::<f::messages::ExportChatInvite>(body) {
            let peer = input_who(&request.peer);
            let idx = self.invites.len() + 1;
            let inv = tl::types::ChatInviteExported {
                revoked: false,
                permanent: false,
                request_needed: request.request_needed,
                link: format!("https://t.me/+invite_{idx}"),
                admin_id: ME,
                date: crate::hub::now() as i32,
                start_date: None,
                expire_date: request.expire_date,
                usage_limit: request.usage_limit,
                usage: Some(0),
                requested: Some(0),
                subscription_expired: None,
                title: request.title.clone(),
                subscription_pricing: None,
            };
            self.log(&format!(
                "ExportChatInvite {} req={} limit={:?}",
                peer.text(),
                request.request_needed,
                request.usage_limit
            ));
            self.invites.push(inv.clone());
            return answer(tl::enums::ExportedChatInvite::ChatInviteExported(inv));
        }
        if let Some(request) = asked::<f::messages::GetExportedChatInvites>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("GetExportedChatInvites {} revoked={}", peer.text(), request.revoked));
            let listed: Vec<tl::enums::ExportedChatInvite> = self
                .invites
                .iter()
                .filter(|i| i.revoked == request.revoked)
                .cloned()
                .map(tl::enums::ExportedChatInvite::ChatInviteExported)
                .collect();
            return answer(tl::enums::messages::ExportedChatInvites::Invites(
                tl::types::messages::ExportedChatInvites {
                    count: listed.len() as i32,
                    invites: listed,
                    users: users(),
                },
            ));
        }
        if let Some(request) = asked::<f::messages::EditExportedChatInvite>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("EditExportedChatInvite {} {} revoked={}", peer.text(), request.link, request.revoked));
            let found = self
                .invites
                .iter_mut()
                .find(|i| i.link == request.link)
                .map(|i| {
                    i.revoked = request.revoked;
                    i.clone()
                })
                .unwrap_or_else(|| tl::types::ChatInviteExported {
                    revoked: request.revoked,
                    permanent: false,
                    request_needed: false,
                    link: request.link.clone(),
                    admin_id: ME,
                    date: crate::hub::now() as i32,
                    start_date: None,
                    expire_date: None,
                    usage_limit: None,
                    usage: Some(0),
                    requested: Some(0),
                    subscription_expired: None,
                    title: None,
                    subscription_pricing: None,
                });
            return answer(tl::enums::messages::ExportedChatInvite::Invite(
                tl::types::messages::ExportedChatInvite {
                    invite: tl::enums::ExportedChatInvite::ChatInviteExported(found),
                    users: users(),
                },
            ));
        }
        if let Some(request) = asked::<f::messages::GetChatInviteImporters>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("GetChatInviteImporters {} requested={}", peer.text(), request.requested));
            let importers: Vec<tl::enums::ChatInviteImporter> = self
                .join_requests
                .iter()
                .cloned()
                .map(tl::enums::ChatInviteImporter::Importer)
                .collect();
            return answer(tl::enums::messages::ChatInviteImporters::Importers(
                tl::types::messages::ChatInviteImporters {
                    count: importers.len() as i32,
                    importers,
                    users: users(),
                },
            ));
        }
        if let Some(request) = asked::<f::messages::HideChatJoinRequest>(body) {
            let peer = input_who(&request.peer);
            let uid = match request.user_id {
                tl::enums::InputUser::User(u) => u.user_id,
                _ => 0,
            };
            self.log(&format!("HideChatJoinRequest {} u{uid} approved={}", peer.text(), request.approved));
            self.join_requests.retain(|r| r.user_id != uid);
            return answer(updates(vec![tl::types::UpdatePendingJoinRequests {
                peer: peer.peer(),
                requests_pending: self.join_requests.len() as i32,
                recent_requesters: self.join_requests.iter().map(|r| r.user_id).collect(),
            }
            .into()]));
        }
        if let Some(request) = asked::<f::messages::HideAllChatJoinRequests>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("HideAllChatJoinRequests {} approved={}", peer.text(), request.approved));
            self.join_requests.clear();
            return answer(updates(vec![tl::types::UpdatePendingJoinRequests {
                peer: peer.peer(),
                requests_pending: 0,
                recent_requesters: Vec::new(),
            }
            .into()]));
        }
        if let Some(request) = asked::<f::channels::InviteToChannel>(body) {
            let chan_id = match request.channel {
                tl::enums::InputChannel::Channel(c) => c.channel_id,
                _ => 0,
            };
            let uids: Vec<i64> = request
                .users
                .iter()
                .filter_map(|u| match u {
                    tl::enums::InputUser::User(u) => Some(u.user_id),
                    _ => None,
                })
                .collect();
            self.log(&format!("InviteToChannel c{chan_id} {uids:?}"));
            let missing: Vec<tl::enums::MissingInvitee> = uids
                .iter()
                .filter(|&&id| id == SCAM)
                .map(|&user_id| {
                    tl::types::MissingInvitee {
                        premium_would_allow_invite: false,
                        premium_required_for_pm: false,
                        user_id,
                    }
                    .into()
                })
                .collect();
            let result = tl::types::messages::InvitedUsers {
                updates: updates(Vec::new()),
                missing_invitees: missing,
            };
            return answer(tl::enums::messages::InvitedUsers::Users(result));
        }
        if let Some(request) = asked::<f::messages::GetCommonChats>(body) {
            let id = match &request.user_id {
                tl::enums::InputUser::User(user) => user.user_id,
                _ => ME,
            };
            self.log(&format!("GetCommonChats {id}"));
            return answer(tl::enums::messages::Chats::Chats(tl::types::messages::Chats { chats: chats() }));
        }
        if asked::<f::help::GetConfig>(body).is_some() {
            self.log("GetConfig");
            let cfg = tl::types::Config {
                default_p2p_contacts: false,
                preload_featured_stickers: false,
                revoke_pm_inbox: true,
                blocked_mode: false,
                force_try_ipv6: false,
                date: t0(),
                expires: t0() + 3_600,
                test_mode: false,
                this_dc: 2,
                dc_options: Vec::new(),
                dc_txt_domain_name: String::new(),
                chat_size_max: 200,
                megagroup_size_max: 200_000,
                forwarded_count_max: 100,
                online_update_period_ms: 210_000,
                offline_blur_timeout_ms: 5_000,
                offline_idle_timeout_ms: 30_000,
                online_cloud_timeout_ms: 300_000,
                notify_cloud_delay_ms: 30_000,
                notify_default_delay_ms: 1_500,
                push_chat_period_ms: 60_000,
                push_chat_limit: 2,
                edit_time_limit: 172_800,
                revoke_time_limit: i32::MAX,
                revoke_pm_time_limit: i32::MAX,
                rating_e_decay: 2_419_200,
                stickers_recent_limit: 30,
                channels_read_media_period: 604_800,
                tmp_sessions: None,
                call_receive_timeout_ms: 20_000,
                call_ring_timeout_ms: 90_000,
                call_connect_timeout_ms: 30_000,
                call_packet_timeout_ms: 10_000,
                me_url_prefix: "https://t.me/".to_owned(),
                autoupdate_url_prefix: None,
                gif_search_username: Some("gif".to_owned()),
                venue_search_username: None,
                img_search_username: None,
                static_maps_provider: None,
                caption_length_max: 1_024,
                message_length_max: 4_096,
                webfile_dc_id: 4,
                suggested_lang_code: None,
                lang_pack_version: None,
                base_lang_pack_version: None,
                reactions_default: (!self.default_reaction.is_empty()).then(|| {
                    tl::types::ReactionEmoji { emoticon: self.default_reaction.clone() }.into()
                }),
                autologin_token: None,
            };
            return answer(tl::enums::Config::Config(cfg));
        }
        if asked::<f::help::GetAppConfig>(body).is_some() {
            self.log("GetAppConfig");
            return answer(app_config());
        }
        if let Some(request) = asked::<f::account::GetPrivacy>(body) {
            let name = privacy_key_name(&request.key);
            self.log(&format!("account.getPrivacy {name}"));
            let rules = self.privacy.get(name).cloned().unwrap_or_else(|| vec![tl::enums::PrivacyRule::PrivacyValueAllowAll]);
            let found = tl::types::account::PrivacyRules { rules, chats: chats(), users: users() };
            return answer(tl::enums::account::PrivacyRules::Rules(found));
        }
        if let Some(request) = asked::<f::account::SetPrivacy>(body) {
            let name = privacy_key_name(&request.key);
            if self.privacy_refuse {
                self.log(&format!("account.setPrivacy {name} refused"));
                return Err(rpc(403, "PREMIUM_ACCOUNT_REQUIRED", None));
            }
            let mut mapped: Vec<tl::enums::PrivacyRule> = Vec::new();
            let mut tags: Vec<String> = Vec::new();
            for rule in request.rules {
                match rule {
                    tl::enums::InputPrivacyRule::InputPrivacyValueAllowAll => {
                        tags.push("allow_all".into());
                        mapped.push(tl::enums::PrivacyRule::PrivacyValueAllowAll);
                    }
                    tl::enums::InputPrivacyRule::InputPrivacyValueAllowContacts => {
                        tags.push("allow_contacts".into());
                        mapped.push(tl::enums::PrivacyRule::PrivacyValueAllowContacts);
                    }
                    tl::enums::InputPrivacyRule::InputPrivacyValueDisallowAll => {
                        tags.push("disallow_all".into());
                        mapped.push(tl::enums::PrivacyRule::PrivacyValueDisallowAll);
                    }
                    tl::enums::InputPrivacyRule::InputPrivacyValueAllowUsers(u) => {
                        let ids: Vec<i64> = u.users.iter().filter_map(|u| match u {
                            tl::enums::InputUser::User(u) => Some(u.user_id),
                            _ => None,
                        }).collect();
                        tags.push(format!("allow_users:{ids:?}"));
                        mapped.push(tl::types::PrivacyValueAllowUsers { users: ids }.into());
                    }
                    tl::enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(u) => {
                        let ids: Vec<i64> = u.users.iter().filter_map(|u| match u {
                            tl::enums::InputUser::User(u) => Some(u.user_id),
                            _ => None,
                        }).collect();
                        tags.push(format!("disallow_users:{ids:?}"));
                        mapped.push(tl::types::PrivacyValueDisallowUsers { users: ids }.into());
                    }
                    tl::enums::InputPrivacyRule::InputPrivacyValueAllowChatParticipants(c) => {
                        tags.push(format!("allow_chats:{:?}", c.chats));
                        mapped.push(tl::types::PrivacyValueAllowChatParticipants { chats: c.chats }.into());
                    }
                    tl::enums::InputPrivacyRule::InputPrivacyValueDisallowChatParticipants(c) => {
                        tags.push(format!("disallow_chats:{:?}", c.chats));
                        mapped.push(tl::types::PrivacyValueDisallowChatParticipants { chats: c.chats }.into());
                    }
                    _ => {}
                }
            }
            self.log(&format!("account.setPrivacy {name} {}", tags.join(",")));
            self.privacy.insert(name, mapped.clone());
            let found = tl::types::account::PrivacyRules { rules: mapped, chats: chats(), users: users() };
            return answer(tl::enums::account::PrivacyRules::Rules(found));
        }
        if let Some(request) = asked::<f::messages::SetHistoryTtl>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("SetHistoryTtl {} {}", peer.text(), request.period));
            if request.period > 0 {
                self.ttl_periods.insert(peer, request.period);
            } else {
                self.ttl_periods.remove(&peer);
            }
            self.next += 1;
            let service = tl::types::MessageService {
                out: true,
                mentioned: false,
                media_unread: false,
                reactions_are_possible: false,
                silent: false,
                post: false,
                legacy: false,
                id: self.next,
                from_id: Some(Who::User(ME).peer()),
                peer_id: peer.peer(),
                saved_peer_id: None,
                reply_to: None,
                date: crate::hub::now() as i32,
                action: tl::types::MessageActionSetMessagesTtl {
                    period: request.period,
                    auto_setting_from: None,
                }
                .into(),
                reactions: None,
                ttl_period: None,
            };
            self.messages.entry(peer.text()).or_default().insert(service.id, service.clone().into());
            let upd: tl::enums::Update = match peer {
                Who::Channel(_) => tl::types::UpdateNewChannelMessage { message: service.into(), pts: 0, pts_count: 0 }.into(),
                _ => tl::types::UpdateNewMessage { message: service.into(), pts: 0, pts_count: 0 }.into(),
            };
            return answer(updates(vec![upd]));
        }
        if asked::<f::messages::GetDefaultHistoryTtl>(body).is_some() {
            self.log("GetDefaultHistoryTtl");
            return answer(tl::enums::DefaultHistoryTtl::Ttl(tl::types::DefaultHistoryTtl {
                period: self.default_ttl,
            }));
        }
        if let Some(request) = asked::<f::messages::SetDefaultHistoryTtl>(body) {
            self.log(&format!("SetDefaultHistoryTtl {}", request.period));
            self.default_ttl = request.period.max(0);
            return answer(true);
        }
        if asked::<f::account::GetAutoDownloadSettings>(body).is_some() {
            if self.download_unread {
                self.log("account.getAutoDownloadSettings unread");
                return Err(rpc(400, "CONNECTION_NOT_INITED", None));
            }
            self.log("account.getAutoDownloadSettings");
            let found = tl::types::account::AutoDownloadSettings {
                low: tl::enums::AutoDownloadSettings::Settings(self.download[0].clone()),
                medium: tl::enums::AutoDownloadSettings::Settings(self.download[1].clone()),
                high: tl::enums::AutoDownloadSettings::Settings(self.download[2].clone()),
            };
            return answer(tl::enums::account::AutoDownloadSettings::Settings(found));
        }
        if let Some(request) = asked::<f::account::SaveAutoDownloadSettings>(body) {
            if self.download_refuse {
                self.log("account.saveAutoDownloadSettings refused");
                return Err(rpc(403, "PREMIUM_ACCOUNT_REQUIRED", None));
            }
            let tl::enums::AutoDownloadSettings::Settings(settings) = &request.settings;
            let slot = if request.low { 0 } else if request.high { 2 } else { 1 };
            let which = ["low", "medium", "high"][slot];
            self.log(&format!(
                "account.saveAutoDownloadSettings {which} photo={} video={} file={} preload={} bitrate={}",
                settings.photo_size_max, settings.video_size_max, settings.file_size_max,
                u8::from(settings.video_preload_large), settings.video_upload_maxbitrate
            ));
            self.download[slot] = settings.clone();
            return answer(true);
        }
        if let Some(request) = asked::<f::premium::GetBoostsStatus>(body) {
            let Who::Channel(id) = input_who(&request.peer) else {
                self.log("premium.getBoostsStatus refused");
                return Err(rpc(400, "PEER_ID_INVALID", None));
            };
            self.log(&format!("premium.getBoostsStatus c{id}"));
            let url = if id == NOTES { "https://t.me/boost/fieldnotes".to_owned() } else { format!("https://t.me/boost/c{id}") };
            let status = boost_status(self.boost_level, self.boost_count, self.boost_peer == Some(id), &url);
            return answer(tl::enums::premium::BoostsStatus::Status(status));
        }
        if asked::<f::premium::GetMyBoosts>(body).is_some() {
            let free = self.boost_peer.is_none();
            self.log(if free { "premium.getMyBoosts free" } else { "premium.getMyBoosts used" });
            let mine = tl::types::premium::MyBoosts { my_boosts: vec![one_boost(1, self.boost_peer)], chats: Vec::new(), users: Vec::new() };
            return answer(tl::enums::premium::MyBoosts::Boosts(mine));
        }
        if let Some(request) = asked::<f::premium::ApplyBoost>(body) {
            if self.boost_refuse {
                self.log("premium.applyBoost refused");
                return Err(rpc(403, "PREMIUM_ACCOUNT_REQUIRED", None));
            }
            let Who::Channel(id) = input_who(&request.peer) else {
                self.log("premium.applyBoost refused");
                return Err(rpc(400, "PEER_ID_INVALID", None));
            };
            let slot = request.slots.as_ref().and_then(|slots| slots.first().copied()).unwrap_or(1);
            if self.boost_peer.is_some_and(|held| held != id) {
                self.log(&format!("premium.applyBoost c{id} slot={slot} empty"));
                return Err(rpc(400, "BOOSTS_EMPTY", None));
            }
            self.log(&format!("premium.applyBoost c{id} slot={slot}"));
            self.boost_peer = Some(id);
            self.boost_count += 1;
            if self.boost_count >= 20 {
                self.boost_level += 1;
            }
            let mine = tl::types::premium::MyBoosts { my_boosts: vec![one_boost(slot, Some(id))], chats: Vec::new(), users: Vec::new() };
            return answer(tl::enums::premium::MyBoosts::Boosts(mine));
        }
        if let Some(request) = asked::<f::messages::StartBot>(body) {
            if self.start_refuse {
                self.log("messages.startBot refused");
                return Err(rpc(400, "BOT_INVALID", None));
            }
            let bot = match &request.bot {
                tl::enums::InputUser::User(user) => user.user_id,
                tl::enums::InputUser::UserSelf => self.me,
                _ => 0,
            };
            if bot != BOT {
                self.log("messages.startBot refused");
                return Err(rpc(400, "PEER_ID_INVALID", None));
            }
            self.log(&format!("messages.startBot u{bot} {}", request.start_param));
            return answer(updates(Vec::new()));
        }
        if let Some(answered) = self.panes(body, events) {
            return answered;
        }
        // A secret chat is never accepted, started, or discarded from here. Logging the ask is
        // what lets a test see one: an unknown method is otherwise answered and forgotten.
        if asked::<f::messages::AcceptEncryption>(body).is_some() {
            self.log("AcceptEncryption");
        } else if asked::<f::messages::RequestEncryption>(body).is_some() {
            self.log("RequestEncryption");
        } else if asked::<f::messages::DiscardEncryption>(body).is_some() {
            self.log("DiscardEncryption");
        }
        Err(rpc(400, "METHOD_NOT_IN_THE_SCRIPT", None))
    }

    /// `channels.getParticipants`. Workshop has an owner, two admins and a bot. A search for
    /// Nia names her and nobody else. Any other channel answers with an empty page.
    fn roster(&mut self, body: &[u8]) -> Option<Result<Vec<u8>, Fault>> {
        use tl::functions as f;
        let request = asked::<f::channels::GetParticipants>(body)?;
        let tl::enums::InputChannel::Channel(channel) = &request.channel else {
            return Some(Err(rpc(400, "CHANNEL_INVALID", None)));
        };
        let id = channel.channel_id;
        let asked_for = match &request.filter {
            tl::enums::ChannelParticipantsFilter::ChannelParticipantsAdmins => "admins".to_owned(),
            tl::enums::ChannelParticipantsFilter::ChannelParticipantsBots => "bots".to_owned(),
            tl::enums::ChannelParticipantsFilter::ChannelParticipantsRecent => "recent".to_owned(),
            tl::enums::ChannelParticipantsFilter::ChannelParticipantsSearch(search) => format!("search {}", search.q.trim()),
            _ => "other".to_owned(),
        };
        let logged = if asked_for == "search " {
            format!("GetParticipants c{id} search")
        } else {
            format!("GetParticipants c{id} {asked_for}")
        };
        self.log(&logged);
        let rights = || {
            let rights: tl::types::ChatAdminRights = blank(&[&put(0u32)]);
            rights
        };
        let creator = |user_id| {
            tl::enums::ChannelParticipant::from(tl::types::ChannelParticipantCreator { user_id, admin_rights: rights().into(), rank: None })
        };
        let admin = |user_id, rank: &str| {
            tl::enums::ChannelParticipant::from(tl::types::ChannelParticipantAdmin {
                can_edit: false,
                is_self: false,
                user_id,
                inviter_id: None,
                promoted_by: ME,
                date: t0() - 1_000,
                admin_rights: rights().into(),
                rank: Some(rank.to_owned()),
            })
        };
        let member = |user_id| {
            tl::enums::ChannelParticipant::from(tl::types::ChannelParticipant {
                user_id,
                date: t0() - 500,
                subscription_until_date: None,
                rank: None,
            })
        };
        let (participants, users): (Vec<tl::enums::ChannelParticipant>, Vec<tl::enums::User>) = if id != WORKSHOP {
            (Vec::new(), Vec::new())
        } else if asked_for == "admins" {
            (vec![creator(ME), admin(TOBIN, "Guide"), admin(ODALYS, "Quartermaster")], users())
        } else if asked_for == "bots" {
            (vec![member(BOT)], users())
        } else if asked_for == "recent" {
            (vec![creator(ME), admin(TOBIN, "Guide"), admin(ODALYS, "Quartermaster"), member(BOT)], users())
        } else if asked_for.eq_ignore_ascii_case("search nia") {
            (vec![member(NIA)], vec![user(NIA, "Nia", "Pell", "").into()])
        } else {
            (Vec::new(), Vec::new())
        };
        let page = tl::types::channels::ChannelParticipants {
            count: participants.len() as i32,
            participants,
            chats: chats(),
            users,
        };
        Some(answer(tl::enums::channels::ChannelParticipants::Participants(page)))
    }

    /// The requests of the panes, the pickers and sending with files.
    fn panes(&mut self, body: &[u8], events: &Sender<Event>) -> Option<Result<Vec<u8>, Fault>> {
        use tl::functions as f;
        if let Some(request) = asked::<f::messages::Search>(body) {
            if let Some(reactions) = request.saved_reaction.as_ref().filter(|r| !r.is_empty()) {
                let emoji = match &reactions[0] {
                    tl::enums::Reaction::Emoji(e) => e.emoticon.clone(),
                    tl::enums::Reaction::CustomEmoji(c) => format!("custom:{}", c.document_id),
                    _ => String::new(),
                };
                self.log(&format!("SearchSavedTag {emoji}"));
                let msgs = vec![message(Who::User(ME), 2, ME, t0() - 100_000, "Packing list: stove, map, headlamp.").into()];
                return Some(answer(tl::enums::messages::Messages::Messages(tl::types::messages::Messages {
                    messages: msgs,
                    topics: Vec::new(),
                    chats: chats(),
                    users: users(),
                })));
            }
            if let tl::enums::MessagesFilter::InputMessagesFilterPhoneCalls(filter) = &request.filter {
                self.log(&format!("Search calls missed={} offset={}", filter.missed, request.offset_id));
                let mut calls: Vec<tl::enums::Message> = self
                    .messages
                    .values()
                    .flat_map(|held| held.values())
                    .filter(|msg| match msg {
                        tl::enums::Message::Service(svc) => match &svc.action {
                            tl::enums::MessageAction::PhoneCall(call) => {
                                if request.offset_id > 0 && svc.id >= request.offset_id {
                                    return false;
                                }
                                if filter.missed {
                                    !svc.out
                                        && matches!(
                                            (&call.reason, call.duration),
                                            (Some(tl::enums::PhoneCallDiscardReason::Missed), _) | (None, None)
                                        )
                                } else {
                                    true
                                }
                            }
                            _ => false,
                        },
                        _ => false,
                    })
                    .cloned()
                    .collect();
                calls.sort_by_key(|msg| {
                    std::cmp::Reverse(match msg {
                        tl::enums::Message::Service(svc) => svc.id,
                        _ => 0,
                    })
                });
                let total = calls.len() as i32;
                calls.truncate(request.limit.max(0) as usize);
                let slice = tl::types::messages::MessagesSlice {
                    inexact: false,
                    count: total,
                    next_rate: None,
                    offset_id_offset: None,
                    search_flood: None,
                    messages: calls,
                    topics: Vec::new(),
                    chats: chats(),
                    users: users(),
                };
                return Some(answer(tl::enums::messages::Messages::Slice(slice)));
            }
            let peer = input_who(&request.peer);
            let (name, mut count, ids): (&str, i32, Vec<i32>) = match request.filter {
                tl::enums::MessagesFilter::InputMessagesFilterPinned => ("pinned", 1, vec![12]),
                tl::enums::MessagesFilter::InputMessagesFilterPhotoVideo => ("media", 37, vec![14]),
                tl::enums::MessagesFilter::InputMessagesFilterUrl => ("links", 4, vec![17]),
                tl::enums::MessagesFilter::InputMessagesFilterMusic => ("music", 1, Vec::new()),
                tl::enums::MessagesFilter::InputMessagesFilterRoundVoice => ("voice", 1, Vec::new()),
                tl::enums::MessagesFilter::InputMessagesFilterGif => ("gifs", 1, Vec::new()),
                _ => ("files", 0, Vec::new()),
            };
            self.log(&format!("Search {} {name} {}", peer.text(), request.offset_id));
            let held = self.messages.get(&peer.text());
            let mut messages: Vec<tl::enums::Message> = ids.iter().filter_map(|id| held.and_then(|held| held.get(id)).cloned()).collect();
            if matches!(request.filter, tl::enums::MessagesFilter::InputMessagesFilterEmpty) {
                self.log(&format!("SearchText {} {} topic={:?}", peer.text(), request.q, request.top_msg_id));
                let query = request.q.to_lowercase();
                let from_who = request.from_id.as_ref().map(input_who);
                let held = self.messages.get(&key(peer, request.top_msg_id));
                let mut matched: Vec<tl::enums::Message> = held
                    .into_iter()
                    .flat_map(|held| held.values())
                    .filter(|message| match message {
                        tl::enums::Message::Message(msg) => {
                            let text_ok = query.is_empty() || msg.message.to_lowercase().contains(&query);
                            let from_ok = from_who.is_none_or(|want| {
                                msg.from_id.as_ref().is_some_and(|p| Who::of(p) == want)
                            });
                            let min_ok = request.min_date <= 0 || msg.date >= request.min_date;
                            let max_ok = request.max_date <= 0 || msg.date <= request.max_date;
                            text_ok && from_ok && min_ok && max_ok
                        }
                        _ => false,
                    })
                    .cloned()
                    .collect();
                matched.sort_by_key(|message| std::cmp::Reverse(match message {
                    tl::enums::Message::Message(message) => message.id,
                    _ => 0,
                }));
                count = matched.len() as i32;
                if request.offset_id > 0 {
                    matched.retain(|message| match message {
                        tl::enums::Message::Message(msg) => msg.id < request.offset_id,
                        _ => false,
                    });
                }
                matched.truncate(request.limit.max(0) as usize);
                messages = matched;
            }
            if name == "links" {
                let mut linked = message(peer, 17, ODALYS, t0() - 30, "Route map");
                linked.entities = Some(vec![tl::types::MessageEntityTextUrl { offset: 0, length: 5, url: "https://example.org/route".to_owned() }.into()]);
                messages = vec![linked.into()];
            } else if name == "music" {
                let mut track = message(peer, 18, ODALYS, t0() - 25, "");
                let doc = tl::types::Document {
                    id: 6_601,
                    access_hash: 6_608,
                    file_reference: vec![1, 2, 3],
                    date: t0() - 25,
                    mime_type: "audio/mpeg".to_owned(),
                    size: 120_000,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![
                        tl::types::DocumentAttributeAudio {
                            voice: false,
                            duration: 185,
                            title: Some("Alpine Theme".to_owned()),
                            performer: Some("Trail Band".to_owned()),
                            waveform: None,
                        }.into(),
                        tl::types::DocumentAttributeFilename { file_name: "alpine.mp3".to_owned() }.into(),
                    ],
                };
                track.media = Some(tl::types::MessageMediaDocument {
                    nopremium: false,
                    spoiler: false,
                    video: false,
                    round: false,
                    voice: false,
                    document: Some(doc.into()),
                    alt_documents: None,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                }.into());
                messages = vec![track.into()];
            } else if name == "voice" {
                let mut note = message(peer, 19, ODALYS, t0() - 20, "");
                let doc = tl::types::Document {
                    id: 6_602,
                    access_hash: 6_609,
                    file_reference: vec![1, 2, 3],
                    date: t0() - 20,
                    mime_type: "audio/ogg".to_owned(),
                    size: 16_000,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl::types::DocumentAttributeAudio {
                        voice: true,
                        duration: 9,
                        title: None,
                        performer: None,
                        waveform: Some(vec![1, 2, 3]),
                    }.into()],
                };
                note.media = Some(tl::types::MessageMediaDocument {
                    nopremium: false,
                    spoiler: false,
                    video: false,
                    round: false,
                    voice: true,
                    document: Some(doc.into()),
                    alt_documents: None,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                }.into());
                messages = vec![note.into()];
            } else if name == "gifs" {
                let mut animated = message(peer, 21, ODALYS, t0() - 15, "");
                animated.media = Some(tl::types::MessageMediaDocument {
                    nopremium: false,
                    spoiler: false,
                    video: false,
                    round: false,
                    voice: false,
                    document: Some(gif(6_200).into()),
                    alt_documents: None,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                }.into());
                messages = vec![animated.into()];
            }
            let slice = tl::types::messages::MessagesSlice {
                inexact: false,
                count,
                next_rate: None,
                offset_id_offset: None,
                search_flood: None,
                messages,
                topics: Vec::new(),
                chats: chats(),
                users: users(),
            };
            return Some(answer(tl::enums::messages::Messages::Slice(slice)));
        }
        if let Some(request) = asked::<f::messages::GetSearchResultsCalendar>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("GetSearchResultsCalendar {} {}", peer.text(), request.offset_date));
            let mut by_day: BTreeMap<i32, (i32, i32, i32)> = BTreeMap::new();
            let mut total = 0i32;
            if let Some(held) = self.messages.get(&peer.text()) {
                for msg in held.values() {
                    if let tl::enums::Message::Message(m) = msg {
                        if request.offset_date > 0 && m.date > request.offset_date {
                            continue;
                        }
                        let day = (m.date / 86_400) * 86_400;
                        let entry = by_day.entry(day).or_insert((m.id, m.id, 0));
                        entry.0 = entry.0.min(m.id);
                        entry.1 = entry.1.max(m.id);
                        entry.2 += 1;
                        total += 1;
                    }
                }
            }
            let min_date = by_day.keys().next().copied().unwrap_or(0);
            let min_msg_id = by_day.values().map(|(lo, _, _)| *lo).min().unwrap_or(0);
            let periods: Vec<tl::enums::SearchResultsCalendarPeriod> = by_day
                .into_iter()
                .rev()
                .map(|(date, (min_msg_id, max_msg_id, count))| {
                    tl::enums::SearchResultsCalendarPeriod::Period(tl::types::SearchResultsCalendarPeriod {
                        date,
                        min_msg_id,
                        max_msg_id,
                        count,
                    })
                })
                .collect();
            let cal = tl::types::messages::SearchResultsCalendar {
                inexact: false,
                count: total,
                min_date,
                min_msg_id,
                offset_id_offset: None,
                periods,
                messages: Vec::new(),
                chats: chats(),
                users: users(),
            };
            return Some(answer(tl::enums::messages::SearchResultsCalendar::Calendar(cal)));
        }
        if let Some(request) = asked::<f::channels::GetFullChannel>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Some(Err(rpc(400, "CHANNEL_INVALID", None)));
            };
            self.log(&format!("GetFullChannel c{}", channel.channel_id));
            let mut full: tl::types::ChannelFull = blank(&[
                &put(0u32),
                &put(0u32),
                &put(channel.channel_id),
                &put(String::new()),
                &put(0i32),
                &put(0i32),
                &put(0i32),
                &put(tl::enums::Photo::Empty(tl::types::PhotoEmpty { id: 0 })),
                &put(settings(None)),
                &put(Vec::<tl::enums::BotInfo>::new()),
                &put(1i32),
            ]);
            full.about = "Notes from the high country.".to_owned();
            full.ttl_period = self.ttl_periods.get(&Who::Channel(channel.channel_id)).copied().filter(|&p| p > 0);
            // The channel turned reactions off; the forum allows all of them, two different
            // ones a message.
            if channel.channel_id == NOTES {
                full.available_reactions = Some(tl::enums::ChatReactions::None);
            } else {
                full.available_reactions = Some(tl::types::ChatReactionsAll { allow_custom: false }.into());
                full.reactions_limit = Some(2);
                full.paid_reactions_available = true;
            }
            // Field Notes stays a large channel. Workshop's count is the four people its
            // member list holds, so no empty Members tail is drawn behind them.
            full.participants_count = Some(if channel.channel_id == WORKSHOP { 4 } else { 1_204 });
            if channel.channel_id == WORKSHOP && std::env::var("TINYCLIENT_TELEGRAM_HIDDEN").ok().as_deref() == Some("1") {
                full.participants_hidden = true;
            }
            full.online_count = Some(230);
            full.slowmode_seconds = self.slowmode_seconds.get(&channel.channel_id).copied();
            full.send_paid_messages_stars = self.paid_stars.get(&channel.channel_id).copied();
            let mut chat_list = chats();
            if self.media_banned.contains(&channel.channel_id) {
                for one in &mut chat_list {
                    if let tl::enums::Chat::Channel(ch) = one {
                        if ch.id == channel.channel_id {
                            let mut banned: tl::types::ChatBannedRights = blank(&[&put(0u32), &put(0i32)]);
                            banned.send_media = true;
                            banned.send_stickers = true;
                            banned.send_gifs = true;
                            banned.send_voices = true;
                            ch.default_banned_rights = Some(banned.into());
                        }
                    }
                }
            }
            let found = tl::types::messages::ChatFull { full_chat: full.into(), chats: chat_list, users: users() };
            return Some(answer(tl::enums::messages::ChatFull::Full(found)));
        }
        if let Some(answered) = self.roster(body) {
            return Some(answered);
        }
        if let Some(request) = asked::<f::channels::GetParticipant>(body) {
            let who = input_who(&request.participant);
            self.log(&format!("GetParticipant {}", who.text()));
            let Who::User(user_id) = who else {
                return Some(Err(rpc(400, "USER_ID_INVALID", None)));
            };
            let rights: tl::types::ChatAdminRights = blank(&[&put(0u32)]);
            let admin = tl::types::ChannelParticipantAdmin {
                can_edit: false,
                is_self: false,
                user_id,
                inviter_id: None,
                promoted_by: TOBIN,
                date: t0() - 1_000,
                admin_rights: rights.into(),
                rank: Some("Quartermaster".to_owned()),
            };
            let found = tl::types::channels::ChannelParticipant { participant: admin.into(), chats: chats(), users: users() };
            return Some(answer(tl::enums::channels::ChannelParticipant::Participant(found)));
        }
        if let Some(request) = asked::<f::account::UpdateNotifySettings>(body) {
            let tl::enums::InputPeerNotifySettings::Settings(settings) = &request.settings;
            let is_muted = settings.mute_until.is_some_and(|v| v > 0);
            let peer = match &request.peer {
                tl::enums::InputNotifyPeer::Peer(peer) => input_who(&peer.peer).text(),
                tl::enums::InputNotifyPeer::InputNotifyForumTopic(topic) => key(input_who(&topic.peer), Some(topic.top_msg_id)),
                tl::enums::InputNotifyPeer::InputNotifyUsers => {
                    self.notify_defaults[0] = is_muted;
                    "users".to_owned()
                }
                tl::enums::InputNotifyPeer::InputNotifyChats => {
                    self.notify_defaults[1] = is_muted;
                    "chats".to_owned()
                }
                tl::enums::InputNotifyPeer::InputNotifyBroadcasts => {
                    self.notify_defaults[2] = is_muted;
                    "broadcasts".to_owned()
                }
            };
            self.log(&format!("UpdateNotifySettings {peer} {}", settings.mute_until.unwrap_or(-1)));
            return Some(answer(true));
        }
        if let Some(request) = asked::<f::channels::JoinChannel>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Some(Err(rpc(400, "CHANNEL_INVALID", None)));
            };
            let id = channel.channel_id;
            self.log(&format!("JoinChannel c{id}"));
            if id == CLOSED {
                return Some(Err(rpc(400, "INVITE_REQUEST_SENT", None)));
            }
            if id == FULL {
                return Some(Err(rpc(400, "CHANNELS_TOO_MUCH", None)));
            }
            let Some(spec) = public_catalog().iter().find(|spec| spec.id == id && spec.joinable) else {
                return Some(Err(rpc(400, "CHANNEL_PRIVATE", None)));
            };
            let mut joined = a_channel(spec.id, spec.title, spec.broadcast, false, spec.username);
            joined.left = false;
            let upd = tl::types::Updates {
                updates: Vec::new(),
                users: users(),
                chats: vec![tl::enums::Chat::Channel(joined)],
                date: t0(),
                seq: 0,
            };
            return Some(answer(tl::enums::messages::ChatInviteJoinResult::Ok(tl::types::messages::ChatInviteJoinResultOk {
                updates: tl::enums::Updates::Updates(upd),
            })));
        }
        if let Some(request) = asked::<f::channels::GetChannelRecommendations>(body) {
            let id = match &request.channel {
                Some(tl::enums::InputChannel::Channel(channel)) => channel.channel_id,
                _ => 0,
            };
            self.log(&format!("GetChannelRecommendations c{id}"));
            let chats = if id == LARCH {
                vec![public_chat(&PublicChat { id: SIMILAR, title: "Similar Peaks", broadcast: true, username: "similarpeaks", joinable: true })]
            } else {
                Vec::new()
            };
            return Some(answer(tl::enums::messages::Chats::Chats(tl::types::messages::Chats { chats })));
        }
        if let Some(request) = asked::<f::channels::LeaveChannel>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Some(Err(rpc(400, "CHANNEL_INVALID", None)));
            };
            self.log(&format!("LeaveChannel c{}", channel.channel_id));
            return Some(answer(updates(Vec::new())));
        }
        if let Some(request) = asked::<f::messages::DeleteChatUser>(body) {
            self.log(&format!("DeleteChatUser g{}", request.chat_id));
            return Some(answer(updates(Vec::new())));
        }
        if let Some(request) = asked::<f::messages::DeleteHistory>(body) {
            let peer = match &request.peer {
                tl::enums::InputPeer::User(user) => format!("u{}", user.user_id),
                tl::enums::InputPeer::Chat(chat) => format!("g{}", chat.chat_id),
                tl::enums::InputPeer::Channel(channel) => format!("c{}", channel.channel_id),
                tl::enums::InputPeer::PeerSelf => "self".to_owned(),
                _ => "other".to_owned(),
            };
            let entry = format!(
                "DeleteHistory {peer} just_clear={} revoke={} max_id={}",
                request.just_clear, request.revoke, request.max_id
            );
            let prior = std::fs::read_to_string(&self.calls)
                .unwrap_or_default()
                .lines()
                .filter(|line| *line == entry)
                .count();
            self.log(&entry);
            // Simulate Telegram's paged offset loop on the first call for just_clear so tests
            // prove the adapter loops while offset != 0.
            let offset = if request.just_clear && prior == 0 { 1 } else { 0 };
            let affected = tl::types::messages::AffectedHistory { pts: 1, pts_count: 0, offset };
            return Some(answer(tl::enums::messages::AffectedHistory::History(affected)));
        }
        if let Some(request) = asked::<f::channels::DeleteHistory>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Some(Err(rpc(400, "CHANNEL_INVALID", None)));
            };
            self.log(&format!(
                "ChannelDeleteHistory c{} for_everyone={} max_id={}",
                channel.channel_id, request.for_everyone, request.max_id
            ));
            return Some(answer(updates(Vec::new())));
        }
        if asked::<f::messages::GetSavedGifs>(body).is_some() {
            self.log("GetSavedGifs");
            let gifs = self.saved_gifs.iter().map(|&id| gif(id).into()).collect();
            let found = tl::types::messages::SavedGifs { hash: 1, gifs };
            return Some(answer(tl::enums::messages::SavedGifs::Gifs(found)));
        }
        if let Some(request) = asked::<f::messages::SaveGif>(body) {
            let doc_id = match &request.id {
                tl::enums::InputDocument::Document(doc) => doc.id,
                tl::enums::InputDocument::Empty => 0,
            };
            self.log(&format!("SaveGif d{doc_id} unsave={}", request.unsave));
            self.saved_gifs.retain(|&id| id != doc_id);
            if !request.unsave && doc_id != 0 {
                self.saved_gifs.insert(0, doc_id);
            }
            return Some(answer(true));
        }
        if let Some(request) = asked::<f::contacts::ResolveUsername>(body) {
            self.log(&format!("ResolveUsername {}", request.username));
            if request.username == OFF_PAGE_HANDLE {
                let group = off_page_group();
                let found = tl::types::contacts::ResolvedPeer {
                    peer: Who::Channel(group.id).peer(), chats: vec![group.into()], users: Vec::new(),
                };
                return Some(answer(tl::enums::contacts::ResolvedPeer::Peer(found)));
            }
            return Some(resolve_username(&request.username));
        }
        if let Some(request) = asked::<f::messages::GetInlineBotResults>(body) {
            let bot = match &request.bot {
                tl::enums::InputUser::User(user) => user.user_id,
                _ => 0,
            };
            self.log(&format!("GetInlineBotResults {} {} {} {}", request.query, request.offset, bot, input_who(&request.peer).text()));
            if request.query == "rust" {
                let send_message = tl::types::BotInlineMessageText {
                    no_webpage: false,
                    invert_media: false,
                    message: "Rust".to_owned(),
                    entities: None,
                    reply_markup: None,
                };
                let result = tl::types::BotInlineResult {
                    id: "a1".to_owned(),
                    r#type: "article".to_owned(),
                    title: Some("Rust".to_owned()),
                    description: Some("A language".to_owned()),
                    url: None,
                    thumb: None,
                    content: None,
                    send_message: send_message.into(),
                };
                let found = tl::types::messages::BotResults {
                    gallery: false,
                    query_id: 88,
                    next_offset: Some("2".to_owned()),
                    switch_pm: None,
                    switch_webview: None,
                    results: vec![result.into()],
                    cache_time: 300,
                    users: Vec::new(),
                };
                return Some(answer(tl::enums::messages::BotResults::Results(found)));
            }
            let send_message = tl::types::BotInlineMessageMediaAuto { invert_media: false, message: String::new(), entities: None, reply_markup: None };
            let result = tl::types::BotInlineMediaResult {
                id: "r1".to_owned(),
                r#type: "gif".to_owned(),
                photo: None,
                document: Some(gif(6_300).into()),
                title: None,
                description: None,
                send_message: send_message.into(),
            };
            let found = tl::types::messages::BotResults {
                gallery: true,
                query_id: 77,
                next_offset: Some("2".to_owned()),
                switch_pm: None,
                switch_webview: None,
                results: vec![result.into()],
                cache_time: 300,
                users: Vec::new(),
            };
            return Some(answer(tl::enums::messages::BotResults::Results(found)));
        }
        if let Some(request) = asked::<f::messages::SendInlineBotResult>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("SendInlineBotResult {} {} {}", peer.text(), request.query_id, request.id));
            let media = document_media(gif(6_300));
            return Some(self.deliver(events, peer, None, None, vec![(request.random_id, String::new(), Some(media), None)], request.schedule_date));
        }
        if asked::<f::messages::GetAllStickers>(body).is_some() {
            self.log("GetAllStickers");
            let mut sets = vec![installed_pack(900, 901, "Hiking", "hiking", 2)];
            if self.camp_installed {
                sets.push(installed_pack(920, 921, "Camp", "camp", 1));
            }
            if self.gold_installed {
                sets.push(installed_pack(930, 931, "Gold", "gold", 1));
            }
            if !self.sticker_order.is_empty() {
                let order = self.sticker_order.clone();
                sets.sort_by_key(|set| order.iter().position(|id| *id == set.id).unwrap_or(usize::MAX));
            }
            let found = tl::types::messages::AllStickers { hash: 1, sets: sets.into_iter().map(Into::into).collect() };
            return Some(answer(tl::enums::messages::AllStickers::Stickers(found)));
        }
        if asked::<f::messages::GetRecentStickers>(body).is_some() {
            self.log("GetRecentStickers");
            let stickers = if self.show_recent { vec![sticker(6_400).into()] } else { Vec::new() };
            let dates = if self.show_recent { vec![t0()] } else { Vec::new() };
            let found = tl::types::messages::RecentStickers { hash: 0, packs: Vec::new(), stickers, dates };
            return Some(answer(tl::enums::messages::RecentStickers::Stickers(found)));
        }
        if asked::<f::messages::GetFavedStickers>(body).is_some() {
            self.log("GetFavedStickers");
            let stickers = self.faved.iter().copied().map(sticker_by_id).map(Into::into).collect();
            let found = tl::types::messages::FavedStickers { hash: 0, packs: Vec::new(), stickers };
            return Some(answer(tl::enums::messages::FavedStickers::Stickers(found)));
        }
        if asked::<f::messages::GetFeaturedStickers>(body).is_some() {
            self.log("GetFeaturedStickers");
            let (premium, sets) = if self.featured_premium {
                let covered = tl::types::StickerSetCovered {
                    set: pack(930, 931, "Gold", "gold", 1).into(),
                    cover: sticker_in(6_430, "✨", 930, 931).into(),
                };
                (true, vec![covered.into()])
            } else {
                (false, Vec::new())
            };
            let found = tl::types::messages::FeaturedStickers { premium, hash: 1, count: sets.len() as i32, sets, unread: Vec::new() };
            return Some(answer(tl::enums::messages::FeaturedStickers::Stickers(found)));
        }
        if let Some(request) = asked::<f::messages::SearchStickers>(body) {
            self.log("SearchStickers");
            let stickers = if request.q.eq_ignore_ascii_case("camp") {
                vec![sticker_in(6_420, "⛺", 920, 921).into()]
            } else if request.q.eq_ignore_ascii_case("gold") {
                vec![sticker_named(6_431, "✨", "gold").into()]
            } else {
                Vec::new()
            };
            let found = tl::types::messages::FoundStickers { next_offset: None, hash: 1, stickers };
            return Some(answer(tl::enums::messages::FoundStickers::Stickers(found)));
        }
        if let Some(request) = asked::<f::messages::GetStickers>(body) {
            self.log("GetStickers");
            let stickers = if request.emoticon == "⛺" {
                vec![sticker_in(6_420, "⛺", 920, 921).into()]
            } else if request.emoticon == "🥾" {
                vec![sticker(6_400).into()]
            } else if request.emoticon == "✨" {
                vec![sticker_in(6_430, "✨", 930, 931).into()]
            } else {
                Vec::new()
            };
            let found = tl::types::messages::Stickers { hash: 1, stickers };
            return Some(answer(tl::enums::messages::Stickers::Stickers(found)));
        }
        if let Some(request) = asked::<f::messages::InstallStickerSet>(body) {
            let id = set_id(&request.stickerset);
            self.log(&format!("InstallStickerSet {id}"));
            if id == 920 {
                self.camp_installed = true;
            }
            return Some(answer(tl::enums::messages::StickerSetInstallResult::Success));
        }
        if let Some(request) = asked::<f::messages::UninstallStickerSet>(body) {
            let id = set_id(&request.stickerset);
            self.log(&format!("UninstallStickerSet {id}"));
            if id == 920 {
                self.camp_installed = false;
            }
            return Some(answer(true));
        }
        if let Some(request) = asked::<f::messages::ReorderStickerSets>(body) {
            self.log("ReorderStickerSets");
            self.sticker_order = request.order.clone();
            return Some(answer(true));
        }
        if let Some(request) = asked::<f::messages::FaveSticker>(body) {
            let doc_id = match &request.id {
                tl::enums::InputDocument::Document(doc) => doc.id,
                tl::enums::InputDocument::Empty => 0,
            };
            self.log(&format!("FaveSticker d{doc_id} unfave={}", request.unfave));
            self.faved.retain(|&id| id != doc_id);
            if !request.unfave && doc_id != 0 {
                self.faved.insert(0, doc_id);
            }
            return Some(answer(true));
        }
        if asked::<f::messages::GetEmojiStickers>(body).is_some() {
            self.log("GetEmojiStickers");
            let mut set: tl::types::StickerSet = blank(&[&put(0u32), &put(910i64), &put(911i64), &put("Faces".to_owned()), &put("faces".to_owned()), &put(1i32), &put(1i32)]);
            set.installed_date = Some(t0());
            let found = tl::types::messages::AllStickers { hash: 1, sets: vec![set.into()] };
            return Some(answer(tl::enums::messages::AllStickers::Stickers(found)));
        }
        if asked::<f::messages::GetRecentReactions>(body).is_some() {
            self.log("GetRecentReactions");
            let found = tl::types::messages::Reactions {
                hash: 1,
                reactions: vec![tl::types::ReactionCustomEmoji { document_id: 6_403 }.into()],
            };
            return Some(answer(tl::enums::messages::Reactions::Reactions(found)));
        }
        if let Some(request) = asked::<f::messages::GetStickerSet>(body) {
            if matches!(&request.stickerset, tl::enums::InputStickerSet::EmojiDefaultTopicIcons) {
                self.log("GetStickerSet topic-icons");
                let set: tl::types::StickerSet = blank(&[
                    &put(0u32),
                    &put(920i64),
                    &put(921i64),
                    &put("Topic Icons".to_owned()),
                    &put("topic_icons".to_owned()),
                    &put(2i32),
                    &put(1i32),
                ]);
                let found = tl::types::messages::StickerSet {
                    set: set.into(),
                    packs: Vec::new(),
                    keywords: Vec::new(),
                    documents: vec![
                        custom_emoji(6_501, "\u{1F527}").into(),
                        custom_emoji(6_502, "\u{1F4CC}").into(),
                    ],
                };
                return Some(answer(tl::enums::messages::StickerSet::Set(found)));
            }
            let id = set_id(&request.stickerset);
            if id == 910 {
                self.log("GetStickerSet emoji");
                let set = pack(910, 911, "Faces", "faces", 1);
                let found = tl::types::messages::StickerSet {
                    set: set.into(),
                    packs: Vec::new(),
                    keywords: Vec::new(),
                    documents: vec![custom_emoji(6_402, "\u{2B50}").into()],
                };
                return Some(answer(tl::enums::messages::StickerSet::Set(found)));
            }
            self.log("GetStickerSet");
            let (set, documents) = match id {
                920 => (pack(920, 921, "Camp", "camp", 1), vec![sticker_in(6_420, "⛺", 920, 921).into()]),
                930 => (pack(930, 931, "Gold", "gold", 1), vec![sticker_in(6_430, "✨", 930, 931).into()]),
                _ => (pack(900, 901, "Hiking", "hiking", 2), vec![sticker(6_400).into(), sticker(6_401).into()]),
            };
            let found = tl::types::messages::StickerSet { set: set.into(), packs: Vec::new(), keywords: Vec::new(), documents };
            return Some(answer(tl::enums::messages::StickerSet::Set(found)));
        }
        if let Some(request) = asked::<f::upload::SaveFilePart>(body) {
            self.log(&format!("SaveFilePart {} {}", request.file_part, request.bytes.len()));
            return Some(answer(true));
        }
        if let Some(request) = asked::<f::upload::SaveBigFilePart>(body) {
            self.log(&format!("SaveBigFilePart {}/{} {}", request.file_part, request.file_total_parts, request.bytes.len()));
            return Some(answer(true));
        }
        if let Some(request) = asked::<f::messages::UploadMedia>(body) {
            let (kind, media) = held_media(&request.media, self.next + 1_000);
            self.log(&format!("UploadMedia {kind}"));
            return Some(answer(media));
        }
        if let Some(request) = asked::<f::messages::Report>(body) {
            let peer = input_who(&request.peer);
            let opt = String::from_utf8_lossy(&request.option);
            self.log(&format!("messages.report {} {:?} opt={opt}", peer.text(), request.id));
            if request.option.is_empty() {
                let choose = tl::types::ReportResultChooseOption {
                    title: "Choose a reason".to_owned(),
                    options: vec![
                        tl::enums::MessageReportOption::Option(tl::types::MessageReportOption {
                            text: "Spam".to_owned(),
                            option: b"spam".to_vec(),
                        }),
                        tl::enums::MessageReportOption::Option(tl::types::MessageReportOption {
                            text: "Violence".to_owned(),
                            option: b"violence".to_vec(),
                        }),
                        tl::enums::MessageReportOption::Option(tl::types::MessageReportOption {
                            text: "Other".to_owned(),
                            option: b"other".to_vec(),
                        }),
                    ],
                };
                return Some(answer(tl::enums::ReportResult::ChooseOption(choose)));
            }
            return Some(answer(tl::enums::ReportResult::Reported));
        }
        if let Some(request) = asked::<f::account::ReportPeer>(body) {
            let peer = input_who(&request.peer);
            let reason = match request.reason {
                tl::enums::ReportReason::InputReportReasonSpam => "spam",
                tl::enums::ReportReason::InputReportReasonViolence => "violence",
                tl::enums::ReportReason::InputReportReasonIllegalDrugs => "illegal",
                tl::enums::ReportReason::InputReportReasonFake => "fake",
                tl::enums::ReportReason::InputReportReasonPersonalDetails => "personal",
                _ => "other",
            };
            self.log(&format!("account.reportPeer {} {reason} {}", peer.text(), request.message));
            return Some(answer(true));
        }
        if let Some(request) = asked::<f::messages::SearchGlobal>(body) {
            self.log(&format!("SearchGlobal {} rate={} id={}", request.q, request.offset_rate, request.offset_id));
            let needle = request.q.to_lowercase();
            let mut matched: Vec<(i32, i32, tl::enums::Message)> = Vec::new();
            for msgs in self.messages.values() {
                for raw in msgs.values() {
                    if let tl::enums::Message::Message(m) = raw {
                        if needle.is_empty() || m.message.to_lowercase().contains(&needle) {
                            matched.push((m.date, m.id, raw.clone()));
                        }
                    }
                }
            }
            matched.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
            let total = matched.len() as i32;
            if request.offset_rate > 0 {
                matched.retain(|(date, id, _)| {
                    *date < request.offset_rate || (*date == request.offset_rate && *id < request.offset_id)
                });
            }
            // Return 1 hit per page when more than 1 matches so pagination is exercised on the wire.
            let page_size = if matched.len() > 1 { 1 } else { matched.len() };
            let next_rate = (matched.len() > page_size).then(|| matched[page_size - 1].0);
            let page: Vec<tl::enums::Message> = matched.into_iter().take(page_size).map(|(_, _, m)| m).collect();
            let slice = tl::types::messages::MessagesSlice {
                inexact: false,
                count: total,
                next_rate,
                offset_id_offset: None,
                search_flood: None,
                messages: page,
                topics: Vec::new(),
                chats: chats(),
                users: users(),
            };
            return Some(answer(tl::enums::messages::Messages::Slice(slice)));
        }
        if let Some(request) = asked::<f::messages::ForwardMessages>(body) {
            let from_who = input_who(&request.from_peer);
            let to_who = input_who(&request.to_peer);
            let to_label = match request.to_peer {
                tl::enums::InputPeer::PeerSelf => "self".to_owned(),
                _ => to_who.text(),
            };
            self.log(&format!("ForwardMessages {} -> {} ids={:?}", from_who.text(), to_label, request.id));
            if self.noforwards.contains(&from_who) {
                return Some(Err(Fault::Rpc { code: 400, name: "CHAT_FORWARDS_RESTRICTED".to_owned(), value: None }));
            }
            let mut all = Vec::new();
            for (idx, msg_id) in request.id.iter().enumerate() {
                let text = self
                    .messages
                    .get(&from_who.text())
                    .and_then(|map| map.get(msg_id))
                    .and_then(|m| match m {
                        tl::enums::Message::Message(m) => Some(m.message.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| format!("forwarded #{msg_id}"));
                self.next += 1;
                let id = self.next;
                let mut sent = message(to_who, id, ME, crate::hub::now() as i32, &text);
                if !request.drop_author {
                    let mut fwd: tl::types::MessageFwdHeader = blank(&[&put(0u32), &put(t0())]);
                    fwd.from_id = Some(from_who.peer());
                    sent.fwd_from = Some(fwd.into());
                }
                if let Some(topic) = request.top_msg_id {
                    sent = in_topic(sent, topic);
                }
                self.store(request.top_msg_id, sent.clone());
                let random_id = request.random_id.get(idx).copied().unwrap_or(i64::from(id));
                let named: tl::enums::Update = tl::types::UpdateMessageId { id, random_id }.into();
                let new: tl::enums::Update = match to_who {
                    Who::Channel(_) => tl::types::UpdateNewChannelMessage { message: sent.into(), pts: 0, pts_count: 0 }.into(),
                    _ => tl::types::UpdateNewMessage { message: sent.into(), pts: 0, pts_count: 0 }.into(),
                };
                let _ = events.send(update(named.clone()));
                let _ = events.send(update(new.clone()));
                all.push(named);
                all.push(new);
            }
            return Some(answer(updates(all)));
        }
        if let Some(request) = asked::<f::messages::SendMedia>(body) {
            let peer = input_who(&request.peer);
            let (kind, media) = held_media(&request.media, self.next + 2_000);
            let options = [("silent", request.silent), ("scheduled", request.schedule_date.is_some())];
            let options: String = options.iter().filter(|(_, on)| *on).map(|(name, _)| format!(" [{name}]")).collect();
            let noted = super::content::entity_note(request.entities.as_deref().unwrap_or(&[]));
            self.log(&format!("SendMedia {} {kind} {}{noted}{options}", peer.text(), request.message));
            if kind.starts_with("voice-note") {
                if let Who::User(id) = peer {
                    if self.voice_forbidden.contains(&id) {
                        return Some(Err(Fault::Rpc {
                            code: 403,
                            name: "VOICE_MESSAGES_FORBIDDEN".to_owned(),
                            value: None,
                        }));
                    }
                } else if self.voice_banned.contains(&peer) {
                    return Some(Err(Fault::Rpc {
                        code: 403,
                        name: "CHAT_SEND_VOICES_FORBIDDEN".to_owned(),
                        value: None,
                    }));
                }
            }
            return Some(self.deliver(events, peer, None, None, vec![(request.random_id, request.message, Some(media), request.entities)], request.schedule_date));
        }
        if let Some(request) = asked::<f::messages::SendMultiMedia>(body) {
            let peer = input_who(&request.peer);
            let mut items = Vec::new();
            let mut kinds = Vec::new();
            for (at, one) in request.multi_media.iter().enumerate() {
                let tl::enums::InputSingleMedia::Media(one) = one;
                let (kind, media) = held_media(&one.media, self.next + 3_000 + at as i32);
                kinds.push(kind);
                items.push((one.random_id, one.message.clone(), Some(media), one.entities.clone()));
            }
            self.log(&format!("SendMultiMedia {} {}", peer.text(), kinds.join(",")));
            return Some(self.deliver(events, peer, None, None, items, request.schedule_date));
        }
        if let Some(request) = asked::<f::messages::TranscribeAudio>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("TranscribeAudio {} {}", peer.text(), request.msg_id));
            if self.transcribe_mode == "exhausted" {
                return Some(Err(Fault::Rpc {
                    code: 403,
                    name: "PREMIUM_ACCOUNT_REQUIRED".to_owned(),
                    value: None,
                }));
            }
            let pending = self.transcribe_mode == "pending";
            let text = if pending {
                "Meet at the".to_owned()
            } else {
                "Meet at the trailhead at seven.".to_owned()
            };
            let trial_remains_num = self.transcribe_trial.map(|n| {
                let next = (n - 1).max(0);
                self.transcribe_trial = Some(next);
                next
            });
            let trial_remains_until_date = trial_remains_num.map(|_| t0() + 7 * 86_400);
            return Some(answer(tl::enums::messages::TranscribedAudio::Audio(
                tl::types::messages::TranscribedAudio {
                    pending,
                    transcription_id: 901,
                    text,
                    trial_remains_num,
                    trial_remains_until_date,
                },
            )));
        }
        if let Some(request) = asked::<f::messages::RateTranscribedAudio>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!(
                "RateTranscribedAudio {} {} {} {}",
                peer.text(),
                request.msg_id,
                request.transcription_id,
                request.good
            ));
            return Some(answer(true));
        }
        None
    }
}

/// A GIF as Telegram keeps one: an animated MP4 with a still.
fn gif(id: i64) -> tl::types::Document {
    tl::types::Document {
        id,
        access_hash: id + 1,
        file_reference: vec![7],
        date: t0(),
        mime_type: "video/mp4".to_owned(),
        size: 120_000,
        thumbs: Some(vec![tl::types::PhotoSize { r#type: "m".to_owned(), w: 320, h: 180, size: 9_000 }.into()]),
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![
            tl::enums::DocumentAttribute::Animated,
            tl::types::DocumentAttributeVideo {
                round_message: false,
                supports_streaming: true,
                nosound: true,
                duration: 3.0,
                w: 320,
                h: 180,
                preload_prefix_size: None,
                video_start_ts: None,
                video_codec: None,
            }
            .into(),
        ],
    }
}

/// A custom emoji document: the alt is what the picker shows and what a send falls back to.
fn custom_emoji(id: i64, alt: &str) -> tl::types::Document {
    tl::types::Document {
        id,
        access_hash: id + 1,
        file_reference: vec![9],
        date: t0(),
        mime_type: "image/webp".to_owned(),
        size: 4_096,
        thumbs: Some(vec![tl::types::PhotoSize { r#type: "m".to_owned(), w: 128, h: 128, size: 2_000 }.into()]),
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![tl::types::DocumentAttributeCustomEmoji {
            free: false,
            text_color: false,
            alt: alt.to_owned(),
            stickerset: tl::types::InputStickerSetId { id: 910, access_hash: 911 }.into(),
        }
        .into()],
    }
}

fn plain_document(id: i64, size: i64, name: &str) -> tl::types::Document {
    tl::types::Document {
        id,
        access_hash: id + 1,
        file_reference: vec![9],
        date: t0(),
        mime_type: "application/octet-stream".to_owned(),
        size,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![tl::types::DocumentAttributeFilename { file_name: name.to_owned() }.into()],
    }
}

fn sticker(id: i64) -> tl::types::Document {
    sticker_in(id, "🥾", 900, 901)
}

fn sticker_in(id: i64, alt: &str, set_id: i64, access: i64) -> tl::types::Document {
    tl::types::Document {
        id,
        access_hash: id + 1,
        file_reference: vec![8],
        date: t0(),
        mime_type: "application/x-tgsticker".to_owned(),
        size: 30_000,
        thumbs: Some(vec![tl::types::PhotoSize { r#type: "m".to_owned(), w: 128, h: 128, size: 4_000 }.into()]),
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![tl::types::DocumentAttributeSticker {
            mask: false,
            alt: alt.to_owned(),
            stickerset: tl::types::InputStickerSetId { id: set_id, access_hash: access }.into(),
            mask_coords: None,
        }
        .into()],
    }
}

fn sticker_named(id: i64, alt: &str, short: &str) -> tl::types::Document {
    let mut document = sticker_in(id, alt, 0, 0);
    document.attributes = vec![tl::types::DocumentAttributeSticker {
        mask: false,
        alt: alt.to_owned(),
        stickerset: tl::types::InputStickerSetShortName { short_name: short.to_owned() }.into(),
        mask_coords: None,
    }
    .into()];
    document
}

fn sticker_by_id(id: i64) -> tl::types::Document {
    match id {
        6_420 => sticker_in(id, "⛺", 920, 921),
        6_430 => sticker_in(id, "✨", 930, 931),
        _ => sticker(id),
    }
}

fn pack(id: i64, access: i64, title: &str, short: &str, count: i32) -> tl::types::StickerSet {
    blank(&[&put(0u32), &put(id), &put(access), &put(title.to_owned()), &put(short.to_owned()), &put(count), &put(1i32)])
}

fn installed_pack(id: i64, access: i64, title: &str, short: &str, count: i32) -> tl::types::StickerSet {
    let mut set = pack(id, access, title, short, count);
    set.installed_date = Some(t0());
    set
}

fn set_id(set: &tl::enums::InputStickerSet) -> i64 {
    match set {
        tl::enums::InputStickerSet::Id(set) => set.id,
        _ => 0,
    }
}

fn document_media(document: tl::types::Document) -> tl::enums::MessageMedia {
    tl::types::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: false,
        round: false,
        voice: false,
        document: Some(document.into()),
        alt_documents: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    }
    .into()
}

/// What Telegram keeps of media a send names, and a word for the log: `photo`, `photo+spoiler`,
/// `document`, `file` (forced), `album-photo` (already kept), `gif`.
fn held_media(media: &tl::enums::InputMedia, id: i32) -> (String, tl::enums::MessageMedia) {
    let id = i64::from(id);
    let photo = |spoiler| -> tl::enums::MessageMedia {
        let tl::enums::Photo::Photo(mut kept) = photo(id) else { unreachable!() };
        kept.id = id;
        tl::types::MessageMediaPhoto { spoiler, live_photo: false, photo: Some(kept.into()), ttl_seconds: None, video: None }.into()
    };
    let file = |name: &str, mime: &str| {
        let mut kept = gif(id);
        kept.mime_type = mime.to_owned();
        kept.thumbs = None;
        kept.attributes = vec![tl::types::DocumentAttributeFilename { file_name: name.to_owned() }.into()];
        document_media(kept)
    };
    match media {
        tl::enums::InputMedia::UploadedPhoto(one) => (if one.spoiler { "photo+spoiler" } else { "photo" }.to_owned(), photo(one.spoiler)),
        tl::enums::InputMedia::UploadedDocument(one) => {
            let audio = one.attributes.iter().find_map(|attribute| match attribute {
                tl::enums::DocumentAttribute::Audio(audio) if audio.voice => Some(audio.clone()),
                _ => None,
            });
            let kind = if one.force_file { "file" } else { "document" };
            let (kind, media) = match audio {
                // A voice note keeps only its audio attribute on the wire, as Telegram's own.
                Some(audio) => {
                    let kept = tl::types::Document {
                        id,
                        access_hash: id + 1,
                        file_reference: vec![9],
                        date: t0(),
                        mime_type: one.mime_type.clone(),
                        size: 18_000,
                        thumbs: None,
                        video_thumbs: None,
                        dc_id: 2,
                        attributes: vec![audio.clone().into()],
                    };
                    (
                        format!("voice-note:{}", audio.duration),
                        tl::types::MessageMediaDocument {
                            nopremium: false,
                            spoiler: false,
                            video: false,
                            round: false,
                            voice: true,
                            document: Some(kept.into()),
                            alt_documents: None,
                            video_cover: None,
                            video_timestamp: None,
                            ttl_seconds: None,
                        }
                        .into(),
                    )
                }
                None => {
                    let name = one.attributes.iter().find_map(|attribute| match attribute {
                        tl::enums::DocumentAttribute::Filename(name) => Some(name.file_name.clone()),
                        _ => None,
                    });
                    let video = one.attributes.iter().find_map(|attribute| match attribute {
                        tl::enums::DocumentAttribute::Video(video) => Some(video),
                        _ => None,
                    });
                    let music = one.attributes.iter().find_map(|attribute| match attribute {
                        tl::enums::DocumentAttribute::Audio(audio) if !audio.voice => Some(audio),
                        _ => None,
                    });
                    let label = if let Some(video) = video {
                        format!(
                            "video:{}x{}:{:.1}:stream={}:thumb={}",
                            video.w,
                            video.h,
                            video.duration,
                            video.supports_streaming,
                            one.thumb.is_some()
                        )
                    } else if let Some(music) = music {
                        format!(
                            "audio:{}:{}",
                            music.title.as_deref().unwrap_or(""),
                            music.performer.as_deref().unwrap_or("")
                        )
                    } else {
                        format!("{kind}:{}", one.mime_type)
                    };
                    (label, file(&name.unwrap_or_default(), &one.mime_type))
                }
            };
            (kind, media)
        }
        tl::enums::InputMedia::Photo(one) => (if one.spoiler { "kept-photo+spoiler" } else { "kept-photo" }.to_owned(), photo(one.spoiler)),
        tl::enums::InputMedia::Dice(one) => (
            format!("dice:{}", one.emoticon),
            tl::types::MessageMediaDice { value: 5, emoticon: one.emoticon.clone(), game_outcome: None }.into(),
        ),
        tl::enums::InputMedia::Document(one) => match &one.id {
            tl::enums::InputDocument::Document(kept) if kept.id == 6_200 => ("gif".to_owned(), document_media(gif(6_200))),
            tl::enums::InputDocument::Document(kept) if kept.id >= 6_400 && kept.id < 6_500 => ("sticker".to_owned(), document_media(sticker(kept.id))),
            _ => ("kept-document".to_owned(), file("file", "application/octet-stream")),
        },
        tl::enums::InputMedia::Contact(one) => (
            format!("contact:{}:{}:{}", one.first_name, one.last_name, one.phone_number),
            tl::types::MessageMediaContact {
                phone_number: one.phone_number.clone(),
                first_name: one.first_name.clone(),
                last_name: one.last_name.clone(),
                vcard: one.vcard.clone(),
                user_id: if one.phone_number.contains("15550100777") { ODALYS } else { 0 },
            }
            .into(),
        ),
        _ => ("other".to_owned(), tl::enums::MessageMedia::Empty),
    }
}

impl Wire for Fake {
    fn invoke(&self, _dc: Option<i32>, body: Vec<u8>) -> Result<Vec<u8>, Fault> {
        let answer = self.world.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).handle(&body, &self.events);
        // An answer the test holds sleeps here, with the world's lock left open so the test
        // can steer and read the calls meanwhile.
        self.hold_back(&body);
        answer
    }

    fn events(&self) -> Option<Receiver<Event>> {
        self.heard.lock().ok()?.take()
    }

    fn home(&self) -> i32 {
        2
    }

    fn set_home(&self, _dc: i32) {}

    fn signed_in(&self, _user: &tl::types::User) {}

    fn session(&self, user: i64, name: &str) -> String {
        serde_json::to_string(&super::Saved { user, name: name.to_owned(), home: 2, dcs: Vec::new() }).unwrap_or_default()
    }

    fn unsaved(&self) -> bool {
        false
    }

    fn close(&self) {
        let _ = self.events.send(Event::Closed("closed".to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scripted_answer_reads_back() {
        let mut world = World::new(true);
        let (events, _heard) = channel();
        let asked = tl::functions::messages::GetDialogs { exclude_pinned: false, folder_id: Some(0), offset_date: 0, offset_id: 0, offset_peer: tl::enums::InputPeer::Empty, limit: 100, hash: 0 };
        for user in users() {
            tl::enums::User::from_bytes(&user.to_bytes()).unwrap();
        }
        for chat in chats() {
            tl::enums::Chat::from_bytes(&chat.to_bytes()).unwrap();
        }
        for held in world.messages.values().flat_map(|held| held.values()) {
            tl::enums::Message::from_bytes(&held.to_bytes()).unwrap();
        }
        let body = world.handle(&asked.to_bytes(), &events).unwrap();
        tl::enums::messages::Dialogs::from_bytes(&body).unwrap();
    }
}
