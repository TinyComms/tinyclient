//! Sample data for looking at the shell with no account signed in. Everything here is invented.
//!
//! ```text
//! TINYCHAT_DIR=/tmp/tinyclient-demo chatd --seed-demo            # the data, then serve it
//! TINYCHAT_DIR=/tmp/tinyclient-demo chatd --seed-demo=expired    # one Slack workspace is Expired
//! TINYCHAT_DIR=/tmp/tinyclient-demo chatd --seed-demo=live       # and things keep happening
//! TINYCHAT_DIR=/tmp/tinyclient-demo chatd --seed-demo=live,expired
//! TINYCHAT_DIR=/tmp/tinyclient-demo chatd --seed-demo=many         # 50 Discord servers
//! TINYCHAT_DIR=/tmp/tinyclient-demo chatd --seed-demo=attention    # Discord wants a verification
//! TINYCHAT_DIR=/tmp/tinyclient-demo chatd --seed-demo=reconnecting # one Slack workspace is retrying
//! ```
//!
//! Refused without `TINYCHAT_DIR`, so it can never write to the real store. Each run replaces
//! what the last one seeded. What is in it:
//!
//! - Discord (connected) as Robin Vale: eight servers (one with a very long name, one with emoji
//!   in it), "Moss & Lichen" with 40 channels in categories (voice channels with people in them,
//!   muted, private and read-only channels, a forum, a thread), direct and group conversations
//!   in every unread state, 25 friends across every presence, two pending requests. `#showcase`
//!   in Moss & Lichen is starred and holds one of every kind of message; `#general` in
//!   Rustaceans Anonymous holds as much history as retention keeps, for paging. Every server has
//!   a `#general`, so a row has to say which Space it is in.
//! - Slack (connected) with two workspaces as two accounts, Harbor Labs and Fernhill Collective.
//!   Their direct and group conversations are in the workspace's tree, as Slack has them.
//! - Telegram (connected): one flat chat list with no Space: pins, folders as filters (Work,
//!   Family, Channels), an archive, a channel with a sponsored message, a forum with topics,
//!   Saved Messages, the 777000 service chat, a bot, a scam account, and ✓ / ✓✓ on our own
//!   last messages.
//! - Teams (connected): chats with no Space (one-to-one, untitled and titled groups, meeting
//!   chats older and newer than 14 days, two app chats, the Notes self-chat, a hidden chat,
//!   two rows with no preview yet), two teams with shown and hidden channels and a channel post
//!   with replies, and people who are Busy, presenting or out of office.
//! - Across them (`polish`): Discord server folders and friends in voice, playing and
//!   listening; a message request; app DMs on Discord and Slack; a Slack section that shows
//!   unread only and channels quiet for 30 days; titles, time zones and tags; what each
//!   account may send; custom emoji, stickers and GIFs for the pickers.
//! - Pictures are real PNG files made here. Most are on disk; a few are left unfetched and land
//!   through the media cache from a server on localhost when the shell asks, and some fail.
//!
//! `live` adds, every few seconds in turn: someone typing, a new message, a reaction, an edit, a
//! presence change (and someone in the Campfire voice channel muting) and a picture landing.
//! Watch the direct conversation with Yusuf Demirci.
//!
//! The demo answers for its accounts itself (`adapter`): a message sent is accepted (with its
//! files, which "upload" in steps that report progress and can be cancelled), a reaction
//! toggles, an edit and a delete work, a friend request can be answered, our own presence set,
//! a person's card and a conversation's or Space's info opened, a member list read, a mute set,
//! a group left, a DM closed, pinned messages and the pickers' catalogs listed, and rows with no
//! preview filled in when they come on screen. A call in a voice channel puts us among its
//! occupants; there is no voice server, so it ends as a call the network never answered, unless
//! `TINYCHAT_DEMO_VOICE` names one (`host:port`), which is how the tests join for real.
//!
//! The data is written with the store calls an adapter's `Context` makes. Links, presence,
//! occupants and everything `live` does need the hub, which exists only once `Chat::new` has
//! run: `seed` leaves them here and the server calls [`attach`].

mod adapter;
mod discord;
mod live;
mod png;
mod polish;
mod serve;
mod showcase;
mod slack;
mod teams;
mod telegram;

use crate::adapters::Adapter;
use crate::hub::Hub;
use crate::store::{ConversationRow, PersonRow, Store, Stored};
use png::Art;
use protocol::{
    AccountKey, Activity, Capabilities, Key, Link, MediaFile, MediaKind, MentionKind, Message, Network, Occupants,
    Presence, Relation, SendState, Sender, Span, Style,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How a message names us.
pub(crate) const MENTION: &str = "@Robin Vale";

/// Our person id on an account.
fn me_of(account: &AccountKey) -> &'static str {
    match account.network {
        Network::Slack if *account == slack::fernhill() => slack::ME_FERNHILL,
        Network::Slack => slack::ME_HARBOR,
        Network::Telegram => telegram::ME,
        Network::Teams => teams::ME,
        Network::Discord => discord::ME,
    }
}

pub type Fallible<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mode {
    pub expired: bool,
    pub live: bool,
    /// Fifty Discord servers, for a rail that has to scroll.
    pub many: bool,
    /// Discord needs something done in its own app.
    pub attention: bool,
    /// One Slack workspace has dropped and is trying again.
    pub reconnecting: bool,
}

/// The demo the command line asks for, if it asks for one.
pub fn requested(args: impl Iterator<Item = String>) -> Option<Mode> {
    let mut mode = None;
    for arg in args {
        let Some(rest) = arg.strip_prefix("--seed-demo") else {
            continue;
        };
        let asked = mode.get_or_insert(Mode::default());
        for word in rest.trim_start_matches('=').split(',') {
            match word {
                "" => {}
                "expired" => asked.expired = true,
                "live" => asked.live = true,
                "many" => asked.many = true,
                "attention" => asked.attention = true,
                "reconnecting" => asked.reconnecting = true,
                other => eprintln!("chatd: --seed-demo: no such variant {other:?} (expired, live, many, attention, reconnecting)"),
            }
        }
    }
    mode
}

/// What the hub is told once it exists.
struct Stage {
    mode: Mode,
    links: Vec<(AccountKey, Link, &'static str)>,
    /// Who we are on each account, for the adapter that answers for it.
    me: Vec<(AccountKey, Who)>,
    presence: Vec<(Key, Presence, String)>,
    activities: Vec<(Key, String, Option<Activity>)>,
    capabilities: Vec<(AccountKey, Capabilities)>,
    occupants: Vec<(Key, Occupants)>,
    pending: showcase::Pending,
    live: live::Plan,
}

static STAGE: Mutex<Option<Stage>> = Mutex::new(None);

/// Someone who says things in the demo.
#[derive(Debug, Clone)]
pub(crate) struct Who {
    pub id: String,
    pub name: String,
    pub avatar: Option<MediaFile>,
    pub bot: bool,
    pub me: bool,
}

impl Who {
    pub fn sender(&self) -> Sender {
        Sender {
            id: self.id.clone(),
            name: self.name.clone(),
            avatar: self.avatar.clone(),
            bot: self.bot,
        }
    }
}

/// One person as the tables below list them: name, handle, presence, status text, relation.
pub(crate) type Listed = (&'static str, &'static str, Presence, &'static str, Relation);

/// Text with its styled ranges, built left to right so the byte offsets are right by making.
#[derive(Default)]
pub(crate) struct Rich {
    pub text: String,
    pub spans: Vec<Span>,
}

impl Rich {
    pub fn plain(mut self, text: &str) -> Rich {
        self.text.push_str(text);
        self
    }

    pub fn styled(self, text: &str, style: Style) -> Rich {
        self.nested(text, [style])
    }

    pub fn nested(mut self, text: &str, styles: impl IntoIterator<Item = Style>) -> Rich {
        let start = self.text.len() as u32;
        self.text.push_str(text);
        let end = self.text.len() as u32;
        self.spans.extend(styles.into_iter().map(|style| Span { start, end, style }));
        self
    }

    pub fn into(self, mut message: Message) -> Message {
        message.text = self.text;
        message.spans = self.spans;
        message
    }
}

pub(crate) struct Seeder<'a> {
    pub store: &'a mut Store,
    pub now: i64,
    media: PathBuf,
    origin: String,
    random: u64,
    made: u64,
    pub mode: Mode,
    presence: Vec<(Key, Presence, String)>,
    /// Status emoji and what people are doing: live, so told to the hub at `attach`.
    pub activities: Vec<(Key, String, Option<Activity>)>,
    pub capabilities: Vec<(AccountKey, Capabilities)>,
    pub occupants: Vec<(Key, Occupants)>,
}

/// The link that opens a person in the network's own app.
fn link_of(account: &AccountKey, id: &str) -> String {
    match account.network {
        Network::Slack => format!("slack://user?team={}&id={id}", account.account),
        Network::Telegram => format!("tg://user?id={}", id.trim_start_matches('u')),
        Network::Teams => format!("https://teams.microsoft.com/l/chat/0/0?users={id}"),
        Network::Discord => format!("discord://-/users/{id}"),
    }
}

/// A person as the store keeps them.
fn person_row(account: &AccountKey, who: &Who, handle: &str, status: &str) -> PersonRow {
    PersonRow {
        id: who.id.clone(),
        name: who.name.clone(),
        handle: handle.to_owned(),
        avatar: who.avatar.as_ref().map(|file| file.id.clone()),
        bot: who.bot,
        status: status.to_owned(),
        link: link_of(account, &who.id),
    }
}

impl Seeder<'_> {
    /// The same numbers every run.
    pub fn roll(&mut self, below: u64) -> u64 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        self.random % below
    }

    /// A message id that sorts after every one made before it, in the network's own form.
    fn message_id(&mut self, network: Network, sent: i64) -> String {
        self.made += 1;
        match network {
            Network::Slack => format!("{sent}.{:06}", self.made),
            _ => (1_300_000_000_000_000_000 + self.made).to_string(),
        }
    }

    fn register(&mut self, account: &AccountKey, file: &MediaFile, route: &str) -> Fallible<()> {
        let url = format!("{}/{route}", self.origin);
        Ok(self.store.register_media(account, &[(file.clone(), url)])?)
    }

    fn drawn(id: &str, kind: MediaKind, width: u32, height: u32) -> MediaFile {
        MediaFile {
            width,
            height,
            ..MediaFile::new(id, kind)
        }
    }

    fn art_route(art: Art, seed: u32, width: u32, height: u32) -> String {
        format!("art/{}/{seed}/{width}/{height}", art.letter())
    }

    /// What a new picture shows: `art` in a colour of its own.
    pub fn look(&mut self, art: Art) -> (Art, u32) {
        (art, self.roll(360) as u32)
    }

    /// The same picture at twice and three times its size, as a network's image proxy would
    /// serve it: what a 2x screen, or a row drawn larger than the thumbnail, asks for.
    fn larger(&mut self, account: &AccountKey, id: &str, look: (Art, u32), width: u32, height: u32) -> Fallible<()> {
        let (art, seed) = look;
        let sizes = [2, 3].into_iter().filter(|times| width.max(height) * times <= 2_400);
        let variants: Vec<(u32, String)> = sizes
            .map(|times| {
                let route = Self::art_route(art, seed, width * times, height * times);
                (width.max(height) * times, format!("{}/{route}?wait=300", self.origin))
            })
            .collect();
        Ok(self.store.register_variants(account, id, &variants)?)
    }

    /// We ourselves, recorded as a person so the account can show our own picture, and present.
    pub fn myself(&mut self, account: &AccountKey, me: &Who) -> Fallible<()> {
        self.store.upsert_people(account, &[person_row(account, me, "robin.vale", "")])?;
        self.presence.push((Key::new(account, me.id.clone()), Presence::Online, String::new()));
        Ok(())
    }

    /// A picture that is already on disk.
    pub fn picture(&mut self, account: &AccountKey, id: &str, kind: MediaKind, look: (Art, u32), width: u32, height: u32) -> Fallible<MediaFile> {
        let (art, seed) = look;
        let mut file = Self::drawn(id, kind, width, height);
        self.register(account, &file, &Self::art_route(art, seed, width, height))?;
        self.larger(account, id, look, width, height)?;
        let path = crate::blobs::path_of(&self.media, account, id);
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")))?;
        let bytes = png::encode(art, seed, width, height);
        std::fs::write(&path, &bytes)?;
        let path = path.to_string_lossy().into_owned();
        self.store.media_landed(account, id, &path, bytes.len() as u64, self.now)?;
        file.bytes = bytes.len() as u64;
        Ok(file)
    }

    /// A picture nobody has fetched. It lands, after a visible wait, when the shell asks.
    pub fn waiting(&mut self, account: &AccountKey, id: &str, kind: MediaKind, look: (Art, u32), width: u32, height: u32) -> Fallible<MediaFile> {
        let (art, seed) = look;
        let file = Self::drawn(id, kind, width, height);
        self.register(account, &file, &format!("{}?wait=900", Self::art_route(art, seed, width, height)))?;
        self.larger(account, id, look, width, height)?;
        Ok(file)
    }

    /// A file whose address answers 404, so asking for it fails.
    pub fn broken(&mut self, account: &AccountKey, id: &str, kind: MediaKind, width: u32, height: u32, bytes: u64) -> Fallible<MediaFile> {
        let mut file = Self::drawn(id, kind, width, height);
        file.bytes = bytes;
        self.register(account, &file, "gone?wait=900")?;
        Ok(file)
    }

    /// A file that is not a picture, served by `route` when the user asks for it.
    pub fn file(&mut self, account: &AccountKey, id: &str, kind: MediaKind, bytes: u64, route: &str) -> Fallible<MediaFile> {
        let mut file = MediaFile::new(id, kind);
        file.bytes = bytes;
        self.register(account, &file, route)?;
        Ok(file)
    }

    /// Records the people of a table and their relationships, and answers them as speakers.
    /// Every fifth has no picture, a few have one not yet fetched and one has one that fails.
    pub fn people(&mut self, account: &AccountKey, prefix: &str, listed: &[Listed]) -> Fallible<Vec<Who>> {
        let mut rows = Vec::new();
        let mut cast = Vec::new();
        let mut relations = Vec::new();
        for (at, (name, handle, presence, status, relation)) in listed.iter().enumerate() {
            let id = format!("{prefix}{:03}", at + 1);
            let avatar_id = format!("avatar-{id}-{:08x}", self.roll(u32::MAX as u64));
            let look = self.look(Art::Person);
            let avatar = match at % 10 {
                4 => None,
                8 => Some(self.waiting(account, &avatar_id, MediaKind::Avatar, look, 80, 80)?),
                9 if at == 19 => Some(self.broken(account, &avatar_id, MediaKind::Avatar, 80, 80, 0)?),
                _ => Some(self.picture(account, &avatar_id, MediaKind::Avatar, look, 80, 80)?),
            };
            let who = Who {
                id: id.clone(),
                name: (*name).to_owned(),
                avatar,
                bot: handle.ends_with("bot"),
                me: false,
            };
            rows.push(person_row(account, &who, handle, status));
            if *relation != Relation::None {
                relations.push((id, *relation));
                self.presence.push((Key::new(account, who.id.clone()), *presence, (*status).to_owned()));
            }
            cast.push(who);
        }
        self.store.upsert_people(account, &rows)?;
        self.store.replace_relations(account, &relations)?;
        Ok(cast)
    }

    /// A message from `who`, `minutes` ago, not yet recorded.
    pub fn message(&mut self, account: &AccountKey, who: &Who, minutes: i64, text: &str) -> Message {
        let sent = self.now - minutes * 60;
        let mut message = Message::plain(self.message_id(account.network, sent), "", text, sent, who.me);
        message.sender = who.sender();
        // Naming us is a mention of us, as the networks' own rules would have it.
        if let (Some(at), false) = (text.find(MENTION), who.me) {
            message.mentions_me = true;
            message.spans.push(Span {
                start: at as u32,
                end: (at + MENTION.len()) as u32,
                style: Style::Mention {
                    kind: MentionKind::User,
                    id: me_of(account).to_owned(),
                },
            });
        }
        message
    }

    pub fn post(&mut self, conversation: &Key, message: &Message) -> Fallible<()> {
        self.store.ingest(conversation, message, false)?;
        Ok(())
    }

    /// Records one plain message and answers its id.
    pub fn say(&mut self, conversation: &Key, who: &Who, minutes: i64, text: &str) -> Fallible<String> {
        let message = self.message(&conversation.account(), who, minutes, text);
        self.post(conversation, &message)?;
        Ok(message.id)
    }

    /// Records a run of plain messages, each `(speaker, minutes ago, text)`, and answers their
    /// ids. The speaker indexes `cast`; anything past its end is us.
    pub fn script(&mut self, conversation: &Key, cast: &[Who], me: &Who, lines: &[(usize, i64, &str)]) -> Fallible<Vec<String>> {
        let mut ids = Vec::with_capacity(lines.len());
        for (speaker, minutes, text) in lines {
            ids.push(self.say(conversation, cast.get(*speaker).unwrap_or(me), *minutes, text)?);
        }
        Ok(ids)
    }

    /// The unread state as a network would report it at connect.
    pub fn counts(&mut self, conversation: &Key, unread: u32, mentions: u32, read: Option<&str>) -> Fallible<()> {
        self.store.set_counts(conversation, unread, mentions, read, 0)?;
        Ok(())
    }

    pub fn conversation(&mut self, account: &AccountKey, row: ConversationRow) -> Fallible<Key> {
        self.store.upsert_conversation(account, &row)?;
        Ok(Key::new(account, row.id))
    }
}

/// Writes the demo into the store and the media directory under `dir`, and leaves the rest for
/// [`attach`].
pub fn seed(store: &mut Store, dir: &Path, mode: Mode) -> Fallible<()> {
    let media = dir.join("media");
    let origin = serve::start()?;
    let mut seeder = Seeder {
        store,
        now: crate::hub::now(),
        media: media.clone(),
        origin: origin.clone(),
        random: 0x7469_6e79_6368_6174,
        made: 0,
        mode,
        presence: Vec::new(),
        activities: Vec::new(),
        capabilities: Vec::new(),
        occupants: Vec::new(),
    };
    // Whatever an earlier run seeded goes first, files included, so ids never collide.
    for account in [discord::account(), slack::harbor(), slack::fernhill(), telegram::account(), teams::account()] {
        seeder.store.forget(&account)?;
        let _ = std::fs::remove_dir_all(crate::blobs::dir_under(&media, &account));
    }
    let discord = discord::seed(&mut seeder)?;
    let showcase = showcase::seed(&mut seeder, &discord)?;
    let mut me = slack::seed(&mut seeder)?;
    me.push((discord.account.clone(), discord.me.clone()));
    me.push((telegram::account(), telegram::seed(&mut seeder)?));
    me.push((teams::account(), teams::seed(&mut seeder)?));
    polish::seed(&mut seeder, &discord)?;

    let fernhill = match (mode.expired, mode.reconnecting) {
        (true, _) => (Link::Expired, "Slack ended this session. Sign in again to send and receive."),
        (false, true) => (Link::Offline, ""),
        (false, false) => (Link::Connected, ""),
    };
    *STAGE.lock().unwrap() = Some(Stage {
        mode,
        links: vec![
            (discord::account(), Link::Connected, ""),
            (slack::harbor(), Link::Connected, ""),
            (slack::fernhill(), fernhill.0, fernhill.1),
            (telegram::account(), Link::Connected, ""),
            (teams::account(), Link::Connected, ""),
        ],
        me,
        presence: seeder.presence,
        activities: seeder.activities,
        capabilities: seeder.capabilities,
        occupants: seeder.occupants,
        pending: showcase,
        live: live::Plan::new(&discord, media, origin),
    });
    Ok(())
}

/// True when a demo was seeded in this process and has not been attached yet, which is when
/// the media cache may fetch from localhost.
pub fn seeded() -> bool {
    STAGE.lock().unwrap().is_some()
}

/// Tells the hub what a seeded demo cannot store (links, presence, who is in voice) and starts
/// what answers for its accounts. Gives those adapters to the server, which asks them as it
/// asks any other. Nothing when no demo was seeded in this process.
pub fn attach(hub: &Arc<Hub>) -> Vec<(AccountKey, Adapter)> {
    let Some(stage) = STAGE.lock().unwrap().take() else {
        return Vec::new();
    };
    let mut adapters = Vec::new();
    for (account, link, detail) in &stage.links {
        match (link, stage.mode.reconnecting) {
            (Link::Offline, true) => hub.set_offline(account, true),
            _ => hub.set_link(account, *link, detail),
        }
        let me = stage.me.iter().find(|(of, _)| of == account).map(|(_, me)| me.clone());
        if let (Link::Connected, Some(me)) = (link, me) {
            match adapter::start(account.clone(), hub.clone(), me) {
                Ok(adapter) => adapters.push((account.clone(), adapter)),
                Err(err) => eprintln!("chatd: demo: {err}"),
            }
        }
    }
    if stage.mode.attention {
        hub.set_required(&discord::account(), "Discord wants you to verify your account. Open Discord, finish the check there, then come back.");
    }
    for (person, presence, status) in stage.presence {
        hub.set_presence(person, presence, &status, true);
    }
    for (person, emoji, activity) in stage.activities {
        hub.set_activity(person, &emoji, activity, true);
    }
    for (account, can) in stage.capabilities {
        hub.set_capabilities(&account, can);
    }
    for (conversation, occupants) in stage.occupants {
        hub.set_occupants(conversation, occupants);
    }
    let hub = hub.clone();
    let spawned = std::thread::Builder::new().name("demo".into()).spawn(move || {
        // `Chat::new` fails whatever is pending when it starts, so the message that is still
        // being sent goes in after it, and is kept young so it never times out.
        std::thread::sleep(Duration::from_millis(500));
        let showcase::Pending { conversation, mut message, unread, mentions } = stage.pending;
        let mut live = stage.mode.live.then_some(stage.live);
        let mut tick = 0u64;
        loop {
            if tick.is_multiple_of(10) {
                message.sent = crate::hub::now();
                message.state = SendState::Pending;
                let mut store = hub.store();
                let stored = store.ingest(&conversation, &message, false);
                if matches!(stored, Ok(Stored::New)) {
                    // Our own new message reads the conversation; put its unread state back.
                    let _ = store.set_counts(&conversation, unread, mentions, None, 0);
                    drop(store);
                    hub.changed(protocol::Change::Messages { conversation: conversation.clone() });
                    hub.changed(protocol::Change::Conversation { conversation: conversation.clone() });
                    hub.changed(protocol::Change::Status);
                }
            }
            // With nothing live to do, only the pending message needs this thread: it wakes
            // for that alone.
            let steps: u32 = match &mut live {
                Some(live) => {
                    live.step(&hub, tick);
                    1
                }
                None => 10,
            };
            tick += u64::from(steps);
            std::thread::sleep(live::STEP * steps);
        }
    });
    if let Err(err) = spawned {
        eprintln!("chatd: demo: {err}");
    }
    adapters
}
