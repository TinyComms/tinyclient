//! A wire with a script behind it and no network: a small invented account that answers the
//! requests the adapter makes and sends updates when a test says so. It exists so the adapter
//! can be driven through chatd's real socket (`tests/telegram.rs`); it is reachable only in a
//! scratch directory with `TINYCHAT_TELEGRAM_FAKE` set, never from a real session.
//!
//! A test steers it by writing lines to a FIFO in the scratch directory: `telegram.signin.ctl`
//! while signing in (`scan`, `scan-password`), `telegram.ctl` once signed in (`incoming`,
//! `photo`, `react`, `edit`, `delete`, `topic`, `typing`, `typing-stop`, `read`, `status`,
//! `poll`, `voice`, `forward`, `flood-send N`, `revoke`). Every request it is sent is appended
//! to `telegram.calls`, one line each, so a test can see what the network would have heard.
//!
//! The account: Mira Kessler (7001); Odalys Brandt (7002, a contact, with a second password on
//! her own number); Tobin Reyes (7003); a group "Trail crew" (8001); a channel "Field Notes"
//! (9001, muted, with a sponsored message); a forum "Workshop" (9002) with topics General and
//! "Tools" (31). The phone +1 555 010 0142 signs in with the code 24680; +1 555 010 0199 also
//! wants the password "correct horse".

use super::peers::Who;
use super::wire::{Event, Fault, Wire};
use grammers_crypto::two_factor_auth::calculate_2fa;
use grammers_tl_types as tl;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use tl::{Deserializable, Identifiable, RemoteCall, Serializable};

const ME: i64 = 7001;
const ODALYS: i64 = 7002;
const TOBIN: i64 = 7003;
const CREW: i64 = 8001;
const NOTES: i64 = 9001;
const WORKSHOP: i64 = 9002;
const TOOLS: i32 = 31;
const CODE: &str = "24680";
const PASSWORD: &str = "correct horse";
/// 2026-10-13 12:00 UTC, so every run stores the same times.
const T0: i32 = 1_791_892_800;
/// The prime Telegram itself uses for the password proof, with generator 3.
const PRIME: &str = "c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f48198a0aa7c14058229493d22530f4dbfa336f6e0ac925139543aed44cce7c3720fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18faf8c595f642477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4a4a695811051907e162753b56b0f6b410dba74d8a84b2a14b3144e0ef1284754fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9ae4e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f0d8115f635b105ee2e4e15d04b2454bf6f4fadf034b10403119cd8e3b92fcc5b";

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap_or(0)).collect()
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
        user.status = Some(tl::types::UserStatusOnline { expires: T0 + 300 }.into());
        user.photo = Some(tl::types::UserProfilePhoto { has_video: false, personal: false, photo_id: 5_002, stripped_thumb: None, dc_id: 2 }.into());
    }
    if id == TOBIN {
        user.status = Some(tl::enums::UserStatus::Recently(blank(&[&put(0u32)])));
    }
    user
}

fn users() -> Vec<tl::enums::User> {
    vec![
        user(ME, "Mira", "Kessler", "mira_k").into(),
        user(ODALYS, "Odalys", "Brandt", "odalys").into(),
        user(TOBIN, "Tobin", "Reyes", "").into(),
    ]
}

fn a_channel(id: i64, title: &str, broadcast: bool, forum: bool, username: &str) -> tl::types::Channel {
    let mut channel: tl::types::Channel =
        blank(&[&put(0u32), &put(0u32), &put(id), &put(title.to_owned()), &put(tl::enums::ChatPhoto::Empty), &put(T0 - 86_400)]);
    channel.access_hash = Some(id * 1_000 + 9);
    channel.broadcast = broadcast;
    channel.megagroup = !broadcast;
    channel.forum = forum;
    channel.username = (!username.is_empty()).then(|| username.to_owned());
    channel
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
        date: T0 - 86_400,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    };
    vec![
        crew.into(),
        tl::enums::Chat::Channel(a_channel(NOTES, "Field Notes", true, false, "fieldnotes")),
        tl::enums::Chat::Channel(a_channel(WORKSHOP, "Workshop", false, true, "")),
    ]
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

fn settings(mute_until: Option<i32>) -> tl::enums::PeerNotifySettings {
    let mut settings: tl::types::PeerNotifySettings = blank(&[&put(0u32)]);
    settings.mute_until = mute_until;
    settings.into()
}

fn dialog(peer: Who, top: i32, read: i32, unread: i32, pinned: bool, folder: Option<i32>) -> tl::enums::Dialog {
    tl::types::Dialog {
        pinned,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: peer.peer(),
        top_message: top,
        read_inbox_max_id: read,
        read_outbox_max_id: top,
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

fn photo(id: i64) -> tl::enums::Photo {
    tl::types::Photo {
        has_stickers: false,
        id,
        access_hash: id + 1,
        file_reference: vec![1, 2, 3],
        date: T0,
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

fn file_piece(offset: i64, limit: i32) -> Vec<u8> {
    let from = (offset.max(0) as usize).min(FILE_BYTES);
    let to = (from + limit.max(0) as usize).min(FILE_BYTES);
    (from..to).map(|at| (at % 251) as u8).collect()
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
    revoked: bool,
    calls: PathBuf,
}

fn key(peer: Who, topic: Option<i32>) -> String {
    match topic {
        Some(topic) => format!("{}_{topic}", peer.text()),
        None => peer.text(),
    }
}

impl World {
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
            revoked: false,
            calls: protocol::support_dir().join("telegram.calls"),
        };
        let odalys = Who::User(ODALYS);
        world.store(None, message(odalys, 10, ODALYS, T0 - 300, "Are you coming to the ridge on Saturday?"));
        world.store(None, message(odalys, 11, ME, T0 - 240, "Yes. I'll bring the stove."));
        world.store(None, message(odalys, 12, ODALYS, T0 - 60, "Good. Trailhead at seven."));
        world.store(None, message(Who::Group(CREW), 20, TOBIN, T0 - 3_600, "Maps are in the shared folder."));
        world.store(None, message(Who::Channel(NOTES), 5, 0, T0 - 7_200, "Snow line is at 1,900 m this week."));
        world.store(Some(TOOLS), in_topic(message(Who::Channel(WORKSHOP), 41, TOBIN, T0 - 500, "Who has the torque wrench?"), TOOLS));
        world.store(None, message(Who::User(TOBIN), 30, TOBIN, T0 - 90_000, "Thanks for the lift."));
        world.store(None, message(Who::User(ME), 2, ME, T0 - 100_000, "Packing list: stove, map, headlamp."));
        world
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

    fn me(&self) -> tl::enums::auth::Authorization {
        tl::types::auth::Authorization {
            setup_password_required: false,
            otherwise_relogin_days: None,
            tmp_sessions: None,
            future_auth_token: None,
            user: user(ME, "Mira", "Kessler", "mira_k").into(),
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
            has_recovery: false,
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
    fn page(&self, place: &str, offset_id: i32, add_offset: i32, limit: i32) -> tl::enums::messages::Messages {
        let all: Vec<&tl::enums::Message> = self.messages.get(place).map(|held| held.values().rev().collect()).unwrap_or_default();
        let id = |message: &tl::enums::Message| match message {
            tl::enums::Message::Message(message) => message.id,
            tl::enums::Message::Service(message) => message.id,
            tl::enums::Message::Empty(message) => message.id,
        };
        let start = match offset_id {
            0 => 0,
            offset => all.iter().position(|message| id(message) < offset).unwrap_or(all.len()) as i32,
        };
        let from = (start + add_offset).max(0) as usize;
        let messages = all.into_iter().skip(from).take(limit.max(0) as usize).cloned().collect();
        tl::types::messages::Messages { messages, topics: Vec::new(), chats: chats(), users: users() }.into()
    }
}

fn rpc(code: i32, name: &str, value: Option<u32>) -> Fault {
    Fault::Rpc { code, name: name.to_owned(), value }
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

fn input_who(peer: &tl::enums::InputPeer) -> Who {
    match peer {
        tl::enums::InputPeer::User(user) => Who::User(user.user_id),
        tl::enums::InputPeer::Chat(chat) => Who::Group(chat.chat_id),
        tl::enums::InputPeer::Channel(channel) => Who::Channel(channel.channel_id),
        _ => Who::User(ME),
    }
}

fn updates(updates: Vec<tl::enums::Update>) -> tl::enums::Updates {
    tl::types::Updates { updates, users: users(), chats: chats(), date: T0, seq: 0 }.into()
}

pub struct Fake {
    world: Arc<Mutex<World>>,
    events: Sender<Event>,
    heard: Mutex<Option<Receiver<Event>>>,
    closed: Arc<AtomicBool>,
    control: PathBuf,
}

impl Fake {
    pub fn open(signed_in: bool) -> Fake {
        let name = if signed_in { "telegram.ctl" } else { "telegram.signin.ctl" };
        let control = protocol::support_dir().join(name);
        let (events, heard) = channel();
        let fake = Fake {
            world: Arc::new(Mutex::new(World::new(signed_in))),
            events,
            heard: Mutex::new(Some(heard)),
            closed: Arc::default(),
            control,
        };
        fake.listen();
        fake
    }

    /// Reads the test's lines for as long as this wire lives. Blocked in the FIFO between
    /// lines, so it costs nothing; dropping the wire writes it one last line to end it.
    fn listen(&self) {
        if !self.control.exists() {
            #[cfg(unix)]
            if let Ok(path) = std::ffi::CString::new(self.control.to_string_lossy().as_bytes()) {
                // SAFETY: `path` is a valid C string for the length of the call.
                unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
            }
            #[cfg(windows)]
            {
                let _ = std::fs::OpenOptions::new().create(true).write(true).open(&self.control);
            }
        }
        let (world, events, closed, control) = (self.world.clone(), self.events.clone(), self.closed.clone(), self.control.clone());
        let _ = std::thread::Builder::new().name("telegram-fake".into()).spawn(move || loop {
            let Ok(file) = std::fs::File::open(&control) else {
                return;
            };
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if closed.load(Ordering::Relaxed) {
                    return;
                }
                let mut world = world.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                for event in world.steer(line.trim()) {
                    let _ = events.send(event);
                }
            }
            if closed.load(Ordering::Relaxed) {
                return;
            }
        });
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        // Wakes the reader out of the FIFO. With no reader the open fails, and there is nobody
        // to wake.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut file) = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&self.control) {
                let _ = file.write_all(b"closed\n");
            }
        }
        #[cfg(windows)]
        {
            if let Ok(mut file) = std::fs::OpenOptions::new().write(true).append(true).open(&self.control) {
                let _ = file.write_all(b"closed\n");
            }
        }
    }
}

fn update(update: tl::enums::Update) -> Event {
    Event::Update { update: Box::new(update), users: users(), chats: chats() }
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
            "scan-password" => {
                self.scanned = Some(true);
                vec![update(tl::enums::Update::LoginToken)]
            }
            // Bold on an accented word, a link on an emoji, a mention by id, code, a spoiler.
            "incoming" => {
                let text = "Café at 🦀 dock, ask Tobin: stove.fuel() secret";
                let mut message = message(odalys, 13, ODALYS, T0 + 10, text);
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
            "photo" => {
                let mut message = message(odalys, 14, ODALYS, T0 + 20, "The view from the ridge");
                message.media = Some(tl::types::MessageMediaPhoto { spoiler: false, live_photo: false, photo: Some(photo(6_014)), ttl_seconds: None, video: None }.into());
                self.store(None, message.clone());
                vec![new(message)]
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
            "edit" => {
                let mut message = message(odalys, 13, ODALYS, T0 + 10, "Café at the dock, seven sharp");
                message.edit_date = Some(T0 + 30);
                self.store(None, message.clone());
                vec![update(tl::types::UpdateEditMessage { message: message.into(), pts: 0, pts_count: 0 }.into())]
            }
            "delete" => {
                if let Some(held) = self.messages.get_mut(&odalys.text()) {
                    held.remove(&13);
                }
                vec![update(tl::types::UpdateDeleteMessages { messages: vec![13], pts: 0, pts_count: 1 }.into())]
            }
            "topic" => {
                let message = in_topic(message(Who::Channel(WORKSHOP), 42, TOBIN, T0 + 40, "Found it, bottom drawer."), TOOLS);
                self.store(Some(TOOLS), message.clone());
                vec![update(tl::types::UpdateNewChannelMessage { message: message.into(), pts: 0, pts_count: 0 }.into())]
            }
            "typing" => vec![update(
                tl::types::UpdateUserTyping { user_id: ODALYS, top_msg_id: None, action: tl::enums::SendMessageAction::SendMessageTypingAction }.into(),
            )],
            "typing-stop" => vec![update(
                tl::types::UpdateUserTyping { user_id: ODALYS, top_msg_id: None, action: tl::enums::SendMessageAction::SendMessageCancelAction }.into(),
            )],
            "read" => vec![update(
                tl::types::UpdateReadHistoryInbox { folder_id: None, peer: odalys.peer(), top_msg_id: None, max_id: 14, still_unread_count: 0, pts: 0, pts_count: 0 }.into(),
            )],
            // Odalys goes offline; Tobin's last-seen stays hidden.
            "status" => vec![
                update(tl::types::UpdateUserStatus { user_id: ODALYS, status: tl::types::UserStatusOffline { was_online: T0 }.into() }.into()),
                update(tl::types::UpdateUserStatus { user_id: TOBIN, status: tl::enums::UserStatus::Recently(blank(&[&put(0u32)])) }.into()),
            ],
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
                let mut message = message(Who::Group(CREW), 21, TOBIN, T0 + 50, "");
                message.media = Some(tl::enums::MessageMedia::Poll(Box::new(tl::types::MessageMediaPoll { poll: poll.into(), results: results.into(), attached_media: None })));
                self.store(None, message.clone());
                vec![update(tl::types::UpdateNewMessage { message: message.into(), pts: 0, pts_count: 0 }.into())]
            }
            "voice" => {
                let voice = tl::types::Document {
                    id: 6_100,
                    access_hash: 6_101,
                    file_reference: vec![4],
                    date: T0,
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
                let mut message = message(odalys, 15, ODALYS, T0 + 60, "");
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
            // A post from the channel, forwarded into the direct chat as an answer to message 12.
            "forward" => {
                let mut message = message(odalys, 16, ODALYS, T0 + 70, "Snow line is at 1,900 m this week.");
                let mut from: tl::types::MessageFwdHeader = blank(&[&put(0u32), &put(T0 - 7_200)]);
                from.from_id = Some(Who::Channel(NOTES).peer());
                from.channel_post = Some(5);
                message.fwd_from = Some(from.into());
                let mut reply: tl::types::MessageReplyHeader = blank(&[&put(0u32)]);
                reply.reply_to_msg_id = Some(12);
                message.reply_to = Some(reply.into());
                self.store(None, message.clone());
                vec![new(message)]
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
                    date: T0 + 80,
                    action: tl::types::MessageActionChatEditTitle { title: "Ridge crew".to_owned() }.into(),
                    reactions: None,
                    ttl_period: None,
                };
                vec![update(tl::types::UpdateNewMessage { message: service.into(), pts: 0, pts_count: 0 }.into())]
            }
            "flood-send" => {
                self.flood_send = words.next().and_then(|seconds| seconds.parse().ok());
                Vec::new()
            }
            "revoke" => {
                self.revoked = true;
                vec![Event::Closed("the session was ended from another device".to_owned())]
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
                    let seconds = std::env::var("TINYCHAT_TELEGRAM_FAKE").ok().and_then(|seconds| seconds.parse::<i64>().ok()).unwrap_or(30);
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
                "+15550100142" | "+15550100199" => {
                    self.phone = Some(request.phone_number);
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
                (Some(CODE), Some("+15550100199")) => {
                    self.password_owed = true;
                    Err(rpc(401, "SESSION_PASSWORD_NEEDED", None))
                }
                (Some(CODE), Some(_)) => {
                    self.signed_in = true;
                    answer(self.me())
                }
                (Some("00000"), _) => Err(rpc(400, "PHONE_CODE_EXPIRED", None)),
                _ => Err(rpc(400, "PHONE_CODE_INVALID", None)),
            };
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
            // The proof the right password gives, made the way a client makes it.
            let asked = self.password();
            let (m1, g_a) = calculate_2fa(&[7; 8], &[9; 16], &unhex(PRIME), &3, vec![5; 256], asked.secure_random, PASSWORD.as_bytes());
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
        if asked::<f::updates::GetState>(body).is_some() {
            return answer(tl::enums::updates::State::State(tl::types::updates::State { pts: 1, qts: 0, date: T0, seq: 1, unread_count: 0 }));
        }
        if asked::<f::users::GetUsers>(body).is_some() {
            return answer(vec![tl::enums::User::from(user(ME, "Mira", "Kessler", "mira_k"))]);
        }
        if asked::<f::account::GetNotifySettings>(body).is_some() {
            return answer(settings(None));
        }
        if asked::<f::account::UpdateStatus>(body).is_some() {
            self.log("UpdateStatus");
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::GetDialogs>(body) {
            let newest = |place: &str| self.messages.get(place).and_then(|held| held.values().next_back().cloned());
            let (dialogs, places): (Vec<tl::enums::Dialog>, Vec<String>) = match request.folder_id {
                Some(1) => (vec![dialog(Who::User(TOBIN), 30, 30, 0, false, Some(1))], vec![Who::User(TOBIN).text()]),
                _ if request.offset_id != 0 => (Vec::new(), Vec::new()),
                _ => (
                    vec![
                        dialog(Who::User(ODALYS), 12, 11, 1, true, None),
                        dialog(Who::Group(CREW), 20, 20, 0, false, None),
                        dialog(Who::Channel(NOTES), 5, 0, 1, false, None),
                        dialog(Who::Channel(WORKSHOP), 41, 0, 1, false, None),
                        dialog(Who::User(ME), 2, 2, 0, false, None),
                    ],
                    vec![Who::User(ODALYS).text(), Who::Group(CREW).text(), Who::Channel(NOTES).text(), key(Who::Channel(WORKSHOP), Some(TOOLS)), Who::User(ME).text()],
                ),
            };
            let messages = places.iter().filter_map(|place| newest(place)).collect();
            return answer(tl::enums::messages::Dialogs::Dialogs(tl::types::messages::Dialogs { dialogs, messages, chats: chats(), users: users() }));
        }
        if asked::<f::messages::GetDialogFilters>(body).is_some() {
            let work = tl::types::DialogFilter {
                contacts: false,
                non_contacts: false,
                groups: false,
                broadcasts: false,
                bots: false,
                exclude_muted: false,
                exclude_read: false,
                exclude_archived: false,
                title_noanimate: false,
                id: 2,
                title: tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities { text: "Work".to_owned(), entities: Vec::new() }),
                emoticon: None,
                color: None,
                pinned_peers: Vec::new(),
                include_peers: vec![tl::types::InputPeerChannel { channel_id: WORKSHOP, access_hash: 0 }.into(), tl::types::InputPeerChat { chat_id: CREW }.into()],
                exclude_peers: Vec::new(),
            };
            let filters = vec![tl::enums::DialogFilter::Default, work.into()];
            return answer(tl::enums::messages::DialogFilters::Filters(tl::types::messages::DialogFilters { tags_enabled: false, filters }));
        }
        if asked::<f::contacts::GetContacts>(body).is_some() {
            let contacts = vec![tl::enums::Contact::Contact(tl::types::Contact { user_id: ODALYS, mutual: true })];
            let found = tl::types::contacts::Contacts { contacts, saved_count: 1, users: vec![user(ODALYS, "Odalys", "Brandt", "odalys").into()] };
            return answer(tl::enums::contacts::Contacts::Contacts(found));
        }
        if let Some(request) = asked::<f::contacts::Search>(body) {
            self.log(&format!("contacts.Search {}", request.q));
            let found = tl::types::contacts::Found {
                my_results: Vec::new(),
                results: vec![Who::User(7_010).peer()],
                chats: Vec::new(),
                users: vec![user(7_010, "Wren", "Halloran", "wren").into()],
            };
            return answer(tl::enums::contacts::Found::Found(found));
        }
        if let Some(request) = asked::<f::messages::GetHistory>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("GetHistory {} {} {}", peer.text(), request.offset_id, request.add_offset));
            return answer(self.page(&peer.text(), request.offset_id, request.add_offset, request.limit));
        }
        if let Some(request) = asked::<f::messages::GetReplies>(body) {
            let place = key(input_who(&request.peer), Some(request.msg_id));
            self.log(&format!("GetReplies {place}"));
            return answer(self.page(&place, request.offset_id, request.add_offset, request.limit));
        }
        if let Some(request) = asked::<f::messages::GetForumTopics>(body) {
            let forum = input_who(&request.peer);
            self.log(&format!("GetForumTopics {}", forum.text()));
            let topic = |id: i32, title: &str, top: i32, unread: i32| {
                let mut topic: tl::types::ForumTopic = blank(&[
                    &put(0u32),
                    &put(id),
                    &put(T0 - 80_000),
                    &put(forum.peer()),
                    &put(title.to_owned()),
                    &put(0i32),
                    &put(top),
                    &put(0i32),
                    &put(0i32),
                    &put(unread),
                    &put(0i32),
                    &put(0i32),
                    &put(0i32),
                    &put(Who::User(TOBIN).peer()),
                    &put(settings(None)),
                ]);
                topic.my = false;
                tl::enums::ForumTopic::Topic(topic)
            };
            let newest = self.messages.get(&key(forum, Some(TOOLS))).and_then(|held| held.values().next_back().cloned());
            let top = match &newest {
                Some(tl::enums::Message::Message(message)) => message.id,
                _ => 41,
            };
            let found = tl::types::messages::ForumTopics {
                order_by_create_date: false,
                count: 2,
                topics: vec![topic(TOOLS, "Tools", top, 1), topic(1, "General", 1, 0)],
                messages: newest.into_iter().collect(),
                chats: chats(),
                users: users(),
                pts: 1,
            };
            return answer(tl::enums::messages::ForumTopics::Topics(found));
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
        if let Some(request) = asked::<f::messages::SendMessage>(body) {
            let peer = input_who(&request.peer);
            if let Some(seconds) = self.flood_send.take() {
                self.log(&format!("SendMessage {} refused {seconds}", peer.text()));
                return Err(rpc(420, "FLOOD_WAIT", Some(seconds)));
            }
            if request.message == "refuse me" {
                return Err(rpc(403, "CHAT_WRITE_FORBIDDEN", None));
            }
            let (topic, reply) = match &request.reply_to {
                Some(tl::enums::InputReplyTo::Message(reply)) if peer == Who::Channel(WORKSHOP) => (Some(reply.top_msg_id.unwrap_or(reply.reply_to_msg_id)), None),
                Some(tl::enums::InputReplyTo::Message(reply)) => (None, Some(reply.reply_to_msg_id)),
                _ => (None, None),
            };
            self.log(&format!("SendMessage {} {}", key(peer, topic), request.message));
            self.next += 1;
            let id = self.next;
            let mut sent = message(peer, id, ME, crate::hub::now() as i32, &request.message);
            if let Some(reply_to_msg_id) = reply {
                let mut header: tl::types::MessageReplyHeader = blank(&[&put(0u32)]);
                header.reply_to_msg_id = Some(reply_to_msg_id);
                sent.reply_to = Some(header.into());
            }
            if let Some(topic) = topic {
                sent = in_topic(sent, topic);
            }
            self.store(topic, sent.clone());
            // Telegram answers a send with the id it gave and the message itself, and the
            // library hands the same two to the update stream.
            let named: tl::enums::Update = tl::types::UpdateMessageId { id, random_id: request.random_id }.into();
            let new: tl::enums::Update = match peer {
                Who::Channel(_) => tl::types::UpdateNewChannelMessage { message: sent.into(), pts: 0, pts_count: 0 }.into(),
                _ => tl::types::UpdateNewMessage { message: sent.into(), pts: 0, pts_count: 0 }.into(),
            };
            let _ = events.send(update(named.clone()));
            let _ = events.send(update(new.clone()));
            return answer(updates(vec![named, new]));
        }
        if let Some(request) = asked::<f::messages::EditMessage>(body) {
            let peer = input_who(&request.peer);
            self.log(&format!("EditMessage {} {}", peer.text(), request.id));
            let Some(tl::enums::Message::Message(mut edited)) = self.messages.get(&peer.text()).and_then(|held| held.get(&request.id)).cloned() else {
                return Err(rpc(400, "MESSAGE_ID_INVALID", None));
            };
            if !edited.out {
                return Err(rpc(403, "MESSAGE_AUTHOR_REQUIRED", None));
            }
            edited.message = request.message.unwrap_or_default();
            edited.edit_date = Some(crate::hub::now() as i32);
            self.store(None, edited.clone());
            return answer(updates(vec![tl::types::UpdateEditMessage { message: edited.into(), pts: 0, pts_count: 0 }.into()]));
        }
        if let Some(request) = asked::<f::messages::DeleteMessages>(body) {
            self.log(&format!("DeleteMessages {:?} revoke {}", request.id, request.revoke));
            for held in self.messages.values_mut() {
                request.id.iter().for_each(|id| drop(held.remove(id)));
            }
            return answer(tl::enums::messages::AffectedMessages::Messages(tl::types::messages::AffectedMessages { pts: 2, pts_count: 1 }));
        }
        if let Some(request) = asked::<f::messages::SendReaction>(body) {
            let peer = input_who(&request.peer);
            let sent: Vec<String> = request.reaction.iter().flatten().filter_map(|reaction| match reaction {
                tl::enums::Reaction::Emoji(emoji) => Some(emoji.emoticon.clone()),
                _ => None,
            }).collect();
            self.log(&format!("SendReaction {} {} {}", peer.text(), request.msg_id, sent.join("")));
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
        if let Some(request) = asked::<f::messages::ReadHistory>(body) {
            self.log(&format!("ReadHistory {} {}", input_who(&request.peer).text(), request.max_id));
            return answer(tl::enums::messages::AffectedMessages::Messages(tl::types::messages::AffectedMessages { pts: 3, pts_count: 1 }));
        }
        if let Some(request) = asked::<f::channels::ReadHistory>(body) {
            let tl::enums::InputChannel::Channel(channel) = &request.channel else {
                return Err(rpc(400, "CHANNEL_INVALID", None));
            };
            self.log(&format!("channels.ReadHistory c{} {}", channel.channel_id, request.max_id));
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::ReadDiscussion>(body) {
            self.log(&format!("ReadDiscussion {} {}", key(input_who(&request.peer), Some(request.msg_id)), request.read_max_id));
            return answer(true);
        }
        if let Some(request) = asked::<f::messages::SetTyping>(body) {
            self.log(&format!("SetTyping {}", input_who(&request.peer).text()));
            return answer(true);
        }
        if let Some(request) = asked::<f::upload::GetFile>(body) {
            let what = match &request.location {
                tl::enums::InputFileLocation::InputPhotoFileLocation(photo) => format!("photo {} {}", photo.id, photo.thumb_size),
                tl::enums::InputFileLocation::InputPeerPhotoFileLocation(photo) => format!("avatar {}", photo.photo_id),
                _ => "other".to_owned(),
            };
            self.log(&format!("GetFile {what} at {}", request.offset));
            let piece = tl::types::upload::File { r#type: tl::enums::storage::FileType::FileJpeg, mtime: T0, bytes: file_piece(request.offset, request.limit) };
            return answer(tl::enums::upload::File::File(piece));
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
            let one = |user_id| tl::enums::ChatParticipant::Participant(tl::types::ChatParticipant { user_id, inviter_id: TOBIN, date: T0, rank: None });
            let participants = tl::types::ChatParticipants { chat_id: request.chat_id, participants: vec![one(TOBIN), one(ODALYS), one(ME)], version: 1 };
            let mut full: tl::types::ChatFull = blank(&[
                &put(0u32),
                &put(request.chat_id),
                &put(String::new()),
                &put(tl::enums::ChatParticipants::Participants(participants)),
                &put(settings(None)),
            ]);
            full.about = "Weekend routes.".to_owned();
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
            let found = tl::types::users::UserFull { full_user: full.into(), chats: Vec::new(), users: users() };
            return answer(tl::enums::users::UserFull::Full(found));
        }
        Err(rpc(400, "METHOD_NOT_IN_THE_SCRIPT", None))
    }
}

impl Wire for Fake {
    fn invoke(&self, _dc: Option<i32>, body: Vec<u8>) -> Result<Vec<u8>, Fault> {
        self.world.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).handle(&body, &self.events)
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
