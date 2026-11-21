//! What a message can hold, the same for every network. Adapters translate into this; the shell
//! never sees Slack mrkdwn, Discord markdown, Telegram entities or Teams HTML.

use crate::{is_false, is_zero, is_zero_i64, Key};
use serde::{Deserialize, Serialize};

/// Most people a [`ThreadSummary`] names, for the pictures beside its reply count.
pub const THREAD_FACES: usize = 3;
/// Most fields a [`Card`] holds.
pub const CARD_FIELDS: usize = 10;
/// Most action links a [`Card`] holds.
pub const CARD_ACTIONS: usize = 10;
/// Most bytes of a voice message's waveform.
pub const WAVEFORM_LIMIT: usize = 256;
/// Most choices one poll lists. Signal allows ten.
pub const POLL_OPTIONS: usize = 10;
/// Most voters named on one choice. The count can be higher.
pub const POLL_VOTERS: usize = 200;
/// Most characters a message keeps of the text it said before its last edit.
pub const PRIOR_LIMIT: usize = 4_000;
/// Most people a message's info lists. A larger group keeps the first ones named.
pub const DELIVERIES_LIMIT: usize = 256;
/// Most interactive controls carried on one message (Slack Block Kit actions/inputs/accessories).
pub const MESSAGE_CONTROLS_LIMIT: usize = 32;
/// Most selectable options on a single menu, radio group, or checkbox group.
pub const CONTROL_OPTIONS_LIMIT: usize = 100;

/// Cuts text from a network to at most `chars` characters, so no frame grows with what a
/// stranger typed.
pub fn clip(text: &mut String, chars: usize) {
    if let Some((at, _)) = text.char_indices().nth(chars) {
        text.truncate(at);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A person's or group's picture, at list size.
    Avatar,
    /// A Space's icon.
    Icon,
    /// A custom emoji.
    Emoji,
    /// A still picture: an image, or the poster of a video or GIF, no wider than 800 px.
    Image,
    Video,
    Gif,
    Audio,
    /// Anything else. Fetched only when the user asks.
    File,
}

/// A file chatd can put on disk. The socket carries the path, never the bytes, and chatd never
/// decodes it. `width` and `height` are known before the bytes arrive, so rows never jump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFile {
    /// Unique within the account and stable: an attachment id, or the network's avatar hash. A
    /// changed picture is a new id.
    pub id: String,
    pub kind: MediaKind,
    /// Pixels. 0 when the network did not say.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub width: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub height: u32,
    /// Size of the file. 0 when unknown.
    #[serde(default)]
    pub bytes: u64,
    /// Where the file is, once it is on disk. `None`: ask `Fetch` when it is on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Why the file could not be fetched, in plain words. A failure of the file itself stands:
    /// chatd does not try again until `Fetch` with `retry` clears it. A failure of the line
    /// (no connection, a timeout) is not kept, and chatd fetches the file again when the shell
    /// asks after a while, so the shell asks again now and then rather than stopping for good.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

impl MediaFile {
    pub fn new(id: impl Into<String>, kind: MediaKind) -> MediaFile {
        MediaFile {
            id: id.into(),
            kind,
            width: 0,
            height: 0,
            bytes: 0,
            path: None,
            failed: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionKind {
    User,
    Channel,
    Role,
    /// Everyone, here, channel.
    Everyone,
}

/// How a range of a message's text is drawn. Block styles (`CodeBlock`, `Quote`, `ListItem`)
/// cover whole lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "style", rename_all = "snake_case")]
pub enum Style {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    CodeBlock {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        language: String,
    },
    Quote,
    ListItem {
        ordered: bool,
        depth: u8,
    },
    Spoiler,
    Link {
        url: String,
    },
    /// The text already reads `@Priya` or `#deploys`; `id` is who or what it names.
    Mention {
        kind: MentionKind,
        id: String,
    },
    /// A custom emoji. The text reads `:name:`, which is also the fallback.
    Emoji {
        file: MediaFile,
    },
}

/// A styled range of [`Message::text`]. `start` and `end` are byte offsets into the UTF-8 text
/// and always fall on character boundaries. Ranges may nest and overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    /// The file itself: its `bytes` is the size shown on a file row.
    pub file: MediaFile,
    /// What an image, video or GIF shows inline. Small enough to fetch when on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<MediaFile>,
    /// A few dozen pixels of the picture that came inside the message (Telegram's stripped
    /// thumbnail), on disk as soon as the message is. Drawn soft, stretched to `thumb`'s box,
    /// until `thumb` is ready to draw; never fetched over the network. `None` from every other
    /// network, and from a shell's point of view always optional: a box without it shows the
    /// usual placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mime: String,
    /// Seconds, for audio and video.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub duration: u32,
    /// Hidden until the user asks to see it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub spoiler: bool,
    /// A sticker: drawn at its own size with no frame, and its transparency kept.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sticker: bool,
    /// A voice message's loudness over time, one byte a bar, at most [`WAVEFORM_LIMIT`]. Its
    /// length in seconds is `duration`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waveform: Vec<u8>,
    /// A round video message (Telegram's): drawn in a circle.
    #[serde(default, skip_serializing_if = "is_false")]
    pub round: bool,
    /// View-once media (Signal, Telegram): hidden until explicitly viewed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub view_once: bool,
}

impl Attachment {
    /// An attachment with nothing said about it but its name and file.
    pub fn new(name: impl Into<String>, file: MediaFile) -> Attachment {
        Attachment {
            name: name.into(),
            file,
            thumb: None,
            preview: None,
            mime: String::new(),
            duration: 0,
            spoiler: false,
            sticker: false,
            waveform: Vec::new(),
            round: false,
            view_once: false,
        }
    }
}

/// A link's card, as the network or sender built it. Readers never fetch the linked site;
/// on Tiny servers, only the sender's device fetches the page before sending when link
/// previews are enabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub site: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Only when the network serves it from its own proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<MediaFile>,
}

/// How many reactors a reaction carries: what a network names in the payload it already
/// sends, never a reason to ask again.
pub const REACTION_REACTORS: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    /// The emoji itself, or `:name:` for a custom one.
    pub emoji: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<MediaFile>,
    pub count: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mine: bool,
    /// Who gave it, where the network said: their ids as `Sender::id` spells them, at most
    /// [`REACTION_REACTORS`], in the network's own order. Empty where the network serves
    /// only a count (Discord's message payload, Tiny's history rows) or hides the list
    /// (Telegram in channels and large groups): the chip then stays a number and says so.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub who: Vec<String>,
}

/// One person who reacted to a message (`Request::Reactors`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reactor {
    pub key: Key,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
}

/// The earlier message this one answers, as one line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyTo {
    pub id: String,
    pub sender: String,
    pub snippet: String,
    /// Who wrote it, for their picture. `None` for a webhook, or when the network did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<Key>,
    /// The message answered has been deleted. `snippet` is then empty.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
    /// Thumbnail of a quoted picture or video (`Quote.attachments` on Signal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<MediaFile>,
    /// True when this message answers a Story (`DataMessage.story_context` on Signal, or a
    /// Telegram story reply) rather than a regular message in the conversation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub story: bool,
}

/// The thread hanging off this message. Its replies are not in the conversation's own pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// The thread's conversation id: what the thread pane asks `Messages` for.
    pub id: String,
    pub replies: u32,
    /// Unix seconds of the newest reply, or 0 when its timestamp is unknown.
    pub last: i64,
    /// The latest people to reply, newest first, at most [`THREAD_FACES`], for their pictures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<Key>,
    /// Replies the user has not read. 0 when the network does not say per thread.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unread: u32,
}

/// Where a forwarded message came from. When the forwarder also wrote a note of their own,
/// the note stays in `Message::text` and the forwarded message's body sits in `text` (with
/// `author` naming who originally said it).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forwarded {
    /// The conversation it was first said in: `#announcements`, a person's name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_title: String,
    /// That conversation's Space, when it has one and it is not this one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_space: String,
    /// Opens the original in the network's own app. Empty when it cannot be linked to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link: String,
    /// Who originally wrote the forwarded message, when distinct from the forwarder.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// The forwarded message's own text when kept distinct from a forwarder's note.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

/// A labelled link or action button at the foot of a [`Card`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardAction {
    pub label: String,
    pub url: String,
}

/// What a bot or app posts instead of plain text: an embed, a block of fields. Drawn as one
/// framed card under the message's text. Every string is cut to a fixed length on its way in.
/// A channel's sponsored message is one too, marked `sponsored`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title_link: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Label and value, in the network's order, at most [`CARD_FIELDS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, String)>,
    /// Labelled links or action buttons, in the network's order, at most [`CARD_ACTIONS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<CardAction>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub footer: String,
    /// Full-width image inside the card (when hosted by the network).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<MediaFile>,
    /// Small thumbnail image beside the card's text (when hosted by the network).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<MediaFile>,
    /// The accent the network gave it, `0xRRGGBB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    /// A channel's sponsored message: the shell tells chatd when the user follows it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sponsored: bool,
}

impl Card {
    pub fn bound(&mut self) {
        clip(&mut self.author, 256);
        clip(&mut self.title, 256);
        clip(&mut self.title_link, 2_000);
        clip(&mut self.text, 4_000);
        clip(&mut self.footer, 256);
        self.fields.truncate(CARD_FIELDS);
        for (label, value) in &mut self.fields {
            clip(label, 256);
            clip(value, 1_000);
        }
        self.actions.truncate(CARD_ACTIONS);
        for action in &mut self.actions {
            clip(&mut action.label, 120);
            clip(&mut action.url, 2_000);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageKind {
    #[default]
    Normal,
    /// One grey line: joined, pinned, call started. `text` says it.
    System,
    /// Something tinyclient cannot draw. `text` names it ("Poll: Lunch?") and `link` opens it in the
    /// official app. Never blank, never dropped.
    Unsupported {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        link: String,
    },
}

impl MessageKind {
    fn is_normal(&self) -> bool {
        *self == MessageKind::Normal
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SendState {
    /// On the network. Every incoming message is this.
    #[default]
    Sent,
    /// On its way. The message's `id` is its `client_id` until the network names it.
    Pending,
    /// It did not go, and will not unless the user asks. `reason` is in words.
    Failed { reason: String },
}

impl SendState {
    fn is_sent(&self) -> bool {
        *self == SendState::Sent
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// The person's id on the account. Empty for a webhook or an app with no user.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bot: bool,
    /// Highest coloured role's RGB (`0xRRGGBB`) in this Space (Discord).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_color: Option<u32>,
    /// Highest role icon image in this Space (Discord).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_icon: Option<MediaFile>,
    /// Highest role unicode emoji badge in this Space (Discord).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_emoji: Option<String>,
}

/// One person a message was sent to, and what their phone has said about it. Times are
/// milliseconds since 1970. A read or a view implies it was delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    /// Their account id on this network.
    pub person: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<i64>,
    /// A voice note or a video played.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewed: Option<i64>,
    /// Why it did not reach them. Empty when it did, or when that is not known yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub failed: String,
}

/// One choice on a [`Poll`], and who currently picked it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollChoice {
    pub text: String,
    #[serde(default)]
    pub count: u32,
    /// People who picked this, when the network names them. Shorter than `count` once the list is full.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voters: Vec<String>,
}

/// One person's current vote. `serial` is Signal's `voteCount`: a later vote replaces an earlier
/// one only when it is higher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollBallot {
    pub person: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<u32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub serial: u32,
}

/// A contact card shared in a message (Signal, Telegram).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedContact {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phones: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emails: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub organization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
    /// Optional network person id (e.g. `u12345` on Telegram) to open or message this contact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
}

/// A poll on a message: the question, the choices and their counts, which I picked, and whether
/// it has ended. `ballots` is who voted for what, so a later vote from the same person replaces
/// theirs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poll {
    pub question: String,
    pub choices: Vec<PollChoice>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiple: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub closed: bool,
    /// Unix seconds when the poll closes. `None` if it has no scheduled end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closes_at: Option<i64>,
    /// Votes are anonymous; individual voters are not shown.
    #[serde(default, skip_serializing_if = "is_false")]
    pub anonymous: bool,
    /// Total votes/voters when the network reports it separately from the sum of choices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mine: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ballots: Vec<PollBallot>,
}

impl Poll {
    /// A poll nobody has voted on yet.
    pub fn open(question: impl Into<String>, options: impl IntoIterator<Item = impl Into<String>>, multiple: bool) -> Poll {
        Poll {
            question: question.into(),
            choices: options.into_iter().map(|text| PollChoice { text: text.into(), count: 0, voters: Vec::new() }).collect(),
            multiple,
            closed: false,
            closes_at: None,
            anonymous: false,
            total: None,
            mine: Vec::new(),
            ballots: Vec::new(),
        }
    }

    /// Total votes cast across all choices (or the network-supplied `total` when present).
    pub fn total_votes(&self) -> u32 {
        self.total.unwrap_or_else(|| self.choices.iter().map(|c| c.count).sum())
    }

    /// Compute the next `mine` selection when the user clicks `choice`.
    /// Single-choice polls toggle off if already sole selection, otherwise switch to `[choice]`.
    /// Multi-choice polls toggle `choice` in/out while keeping sorted order.
    pub fn next_votes(&self, choice: u32) -> Vec<u32> {
        if choice as usize >= self.choices.len() {
            return self.mine.clone();
        }
        if self.multiple {
            let mut next = self.mine.clone();
            if let Some(pos) = next.iter().position(|&c| c == choice) {
                next.remove(pos);
            } else {
                next.push(choice);
                next.sort_unstable();
                next.dedup();
            }
            next
        } else if self.mine == [choice] {
            Vec::new()
        } else {
            vec![choice]
        }
    }

    /// Apply a person's vote replacement and recompute choice counts, voter lists, and `mine`.
    pub fn apply_vote(&mut self, person: &str, mut choices: Vec<u32>, me: &str) {
        let len = self.choices.len() as u32;
        choices.retain(|&idx| idx < len);
        choices.sort_unstable();
        choices.dedup();
        if !self.multiple {
            choices.truncate(1);
        }
        if let Some(existing) = self.ballots.iter_mut().find(|b| b.person == person) {
            existing.choices = choices;
            existing.serial = existing.serial.saturating_add(1);
        } else {
            self.ballots.push(PollBallot { person: person.to_owned(), choices, serial: 1 });
        }
        self.ballots.retain(|b| !b.choices.is_empty());
        for choice in &mut self.choices {
            choice.count = 0;
            choice.voters.clear();
        }
        for ballot in &self.ballots {
            for &idx in &ballot.choices {
                if let Some(choice) = self.choices.get_mut(idx as usize) {
                    choice.count = choice.count.saturating_add(1);
                    if !self.anonymous && choice.voters.len() < POLL_VOTERS {
                        choice.voters.push(ballot.person.clone());
                    }
                }
            }
        }
        self.mine = self
            .ballots
            .iter()
            .find(|b| b.person == me)
            .map(|b| b.choices.clone())
            .unwrap_or_default();
        self.total = None;
    }
}

/// Visual tone for an interactive message button.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlStyle {
    #[default]
    Default,
    Primary,
    Danger,
}

impl ControlStyle {
    fn is_default(&self) -> bool {
        *self == ControlStyle::Default
    }
}

/// Optional confirmation prompt before an interactive control fires (Slack Block Kit `confirm`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmDialog {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub confirm: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub deny: String,
}

/// One choice inside a select menu, overflow menu, radio group, or checkbox group.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlOption {
    pub label: String,
    pub value: String,
}

/// What kind of interactive widget a [`MessageControl`] draws and submits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlKind {
    Button {
        /// When set, clicking opens this link directly instead of firing an action callback.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        url: String,
        #[serde(default, skip_serializing_if = "ControlStyle::is_default")]
        style: ControlStyle,
    },
    Select {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        placeholder: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<ControlOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        external: bool,
    },
    DatePicker {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        placeholder: String,
        /// `YYYY-MM-DD` when prefilled.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial: Option<String>,
    },
    TimePicker {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        placeholder: String,
        /// `HH:MM` when prefilled.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial: Option<String>,
    },
    Checkboxes {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<ControlOption>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        selected: Vec<String>,
    },
    RadioButtons {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<ControlOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<String>,
    },
}

/// One interactive Block Kit control attached to a bot/app message (button, menu, picker, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageControl {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub block_id: String,
    pub action_id: String,
    /// Network element type (e.g. `"button"`, `"static_select"`, `"datepicker"`, `"overflow"`).
    pub element_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<ConfirmDialog>,
    pub kind: ControlKind,
}

/// Maximum inline button rows kept on a single message.
pub const MESSAGE_BUTTON_ROWS_LIMIT: usize = 32;
/// Maximum buttons kept in a single inline button row.
pub const MESSAGE_BUTTONS_PER_ROW_LIMIT: usize = 12;

/// What pressing an inline bot button does (Telegram `replyInlineMarkup`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InlineButtonKind {
    /// Fires a bot callback (`messages.getBotCallbackAnswer`); `data` is hex-encoded payload bytes.
    Callback {
        data: String,
        #[serde(default, skip_serializing_if = "is_false")]
        requires_password: bool,
    },
    /// Opens an external link or web view.
    Url { url: String },
    /// Copies text to the clipboard (`keyboardButtonCopy`).
    Copy { text: String },
    /// Switches to an inline bot query (`keyboardButtonSwitchInline`).
    SwitchInline {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        query: String,
        #[serde(default, skip_serializing_if = "is_false")]
        same_peer: bool,
    },
    /// A button kind that must be handled in the official app.
    Unsupported,
}

/// One inline keyboard button under a bot message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineButton {
    pub label: String,
    #[serde(flatten)]
    pub kind: InlineButtonKind,
}

/// Feedback returned by a bot when an inline callback button is pressed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub alert: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// One message. Which conversation it is in comes from the answer that carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub sender: Sender,
    /// Saved legacy plaintext whose sender authentication was not retained. The sender shown
    /// is the wire author or old local attribution: display metadata, never cryptographic
    /// authorization, so the row cannot be edited.
    #[serde(default, skip_serializing_if = "is_false")]
    pub saved_history: bool,
    /// Unix seconds.
    pub sent: i64,
    /// Milliseconds within `sent`, when the source supplies finer ordering.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sent_subsec_millis: u32,
    /// The network's own order, when the clock is coarser than one message. Tiny stamps an
    /// encrypted message to the minute and a plaintext message to the second, then numbers
    /// each post. `0` on a network that has no such number, and on a send that has not been
    /// given one yet.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub seq: i64,
    /// Unix seconds when the message's own timer (Telegram's `ttl_period`) makes it leave the
    /// store, whether the account is online or not. `None`: it does not run out on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub outgoing: bool,
    /// Plain text. Mentions already read as names and no network markup is left in it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previews: Vec<Preview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ReplyTo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadSummary>,
    /// Boxed, like `card`: few messages have one, and every message would carry its size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded: Option<Box<Forwarded>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<Box<Card>>,
    /// Additional stacked cards beyond the first `card` (e.g. multiple Slack attachments).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cards: Vec<Card>,
    /// Boxed, like `card`: few messages are a poll.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll: Option<Box<Poll>>,
    /// A message that was deleted but still has a thread hanging off it. Everything it said
    /// is gone; the row stays so the thread has somewhere to hang.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
    /// Removed by a moderator rather than the author (Tiny `deleted_by`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub moderated: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub edited: bool,
    /// Unix seconds of the last edit, from a network that hands one over. `None`: never
    /// edited, or only the fact is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<i64>,
    /// What the message said before its last edit, at most [`PRIOR_LIMIT`] characters. The
    /// store keeps one prior text when an edit changes the words. `None`: never edited, or
    /// nothing kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_text: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
    /// Who pinned this message, when the network reports it (e.g. Slack `pins.list`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_by: Option<String>,
    /// Unix seconds when this message was pinned, when the network reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<i64>,
    /// Starred on the network (Google Messages).
    #[serde(default, skip_serializing_if = "is_false")]
    pub starred: bool,
    /// How many people saw a channel's post, when the network counts them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub views: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mentions_me: bool,
    #[serde(default, skip_serializing_if = "MessageKind::is_normal")]
    pub kind: MessageKind,
    #[serde(default, skip_serializing_if = "SendState::is_sent")]
    pub state: SendState,
    /// Queued because the account was offline. Still [`SendState::Pending`]. The shell draws a
    /// clock and "Waiting for {network}", not "Sending…". Older clients ignore the field and
    /// show it as on its way.
    #[serde(default, skip_serializing_if = "is_false")]
    pub waiting: bool,
    /// What the shell called it when sending. Set on our own messages only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// End-to-end encrypted and opened, or encrypted and not opened here. `Plain` for every
    /// network that does not say, and for a plaintext message of a Tiny server.
    #[serde(default, skip_serializing_if = "crate::Seal::is_plain")]
    pub seal: crate::Seal,
    /// Who this was sent to, and what each phone has said. Empty when the network does not
    /// say, and on a message that is not ours.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<Delivery>,
    /// Why a read list is empty. "Read receipts are off" when this account does not share them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub receipt_note: String,
    /// Structured metadata for a call transcript row (`MessageKind::System`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<CallNote>,
    /// Per-message edit override when the network decides per message. `None`: derived from
    /// account `Capabilities` and `edit_window`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_edit: Option<bool>,
    /// Per-message delete override when the network decides per message. `None`: derived from
    /// account `Capabilities` and conversation `ConversationRights`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_delete: Option<bool>,
    /// Message importance (Teams: Normal / Important / Urgent).
    #[serde(default, skip_serializing_if = "crate::Importance::is_normal")]
    pub importance: crate::Importance,
    /// Channel announcement post (Teams).
    #[serde(default, skip_serializing_if = "is_false")]
    pub announcement: bool,
    /// Optional message effect identifier (Telegram).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    /// Shared contact cards carried on this message (Signal, Telegram).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<SharedContact>,
    /// Short label of the sending line or identity when reported (e.g. carrier name or "SIM 2").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_via: Option<String>,
    /// Canonical HTTPS permalink for this message when the network supplies one (e.g. Slack
    /// `https://<workspace>.slack.com/archives/<channel>/p<ts>`). Used by "Copy link".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub permalink: String,
    /// Interactive app/bot controls (Slack Block Kit buttons, menus, pickers, checkboxes, radios).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<MessageControl>,
    /// Inline keyboard button rows under a bot message (Telegram `replyInlineMarkup`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<Vec<InlineButton>>,
}

/// How a direct or group call ended, or that it is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallOutcome {
    Missed,
    Declined,
    Cancelled,
    Busy,
    Ended,
    Live,
}

/// Structured call metadata attached to a `MessageKind::System` call row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallNote {
    pub outcome: CallOutcome,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caller_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u32>,
    /// User ids who took part in the call, when the network names them (e.g. Discord).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub video: bool,
}

/// An earlier wording of one message, oldest first when several are kept. Only a network that
/// shows its own history has these. A live edit on any other network does not store the words
/// the sender replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub text: String,
    /// Unix seconds of this wording.
    pub at: i64,
}

impl Message {
    /// The name a row shows for its sender. Mine carry my name from the network once it has
    /// settled; "You" stands in only while a send has no name yet.
    pub fn shown_sender(&self) -> &str {
        if self.outgoing && self.sender.name.is_empty() {
            "You"
        } else {
            &self.sender.name
        }
    }

    /// A message that is only text, which is what an adapter has before it maps anything richer.
    pub fn plain(
        id: impl Into<String>,
        sender: impl Into<String>,
        text: impl Into<String>,
        sent: i64,
        outgoing: bool,
    ) -> Message {
        Message {
            id: id.into(),
            saved_history: false,
            sender: Sender {
                name: sender.into(),
                ..Sender::default()
            },
            sent,
            sent_subsec_millis: 0,
            seq: 0,
            expires_at: None,
            outgoing,
            text: text.into(),
            spans: Vec::new(),
            attachments: Vec::new(),
            previews: Vec::new(),
            reactions: Vec::new(),
            reply_to: None,
            thread: None,
            forwarded: None,
            card: None,
            cards: Vec::new(),
            poll: None,
            deleted: false,
            moderated: false,
            edited: false,
            edited_at: None,
            prior_text: None,
            pinned: false,
            pinned_by: None,
            pinned_at: None,
            starred: false,
            views: None,
            mentions_me: false,
            kind: MessageKind::Normal,
            state: SendState::Sent,
            waiting: false,
            client_id: None,
            seal: crate::Seal::Plain,
            deliveries: Vec::new(),
            receipt_note: String::new(),
            call: None,
            can_edit: None,
            can_delete: None,
            importance: crate::Importance::Normal,
            announcement: false,
            effect: None,
            contacts: Vec::new(),
            sent_via: None,
            permalink: String::new(),
            controls: Vec::new(),
            buttons: Vec::new(),
        }
    }

    /// Where this message sits among the others in its conversation.
    ///
    /// Time including private milliseconds first. A server sequence breaks a tie: Tiny's encrypted messages share a minute
    /// and its plaintext messages share a second, and the id is not the order. A row with no
    /// sequence sorts after sequenced rows at that same time, then by id, shorter first.
    pub fn rank(&self) -> (i64, u32, u8, i64, usize, String) {
        (self.sent, self.sent_subsec_millis, u8::from(self.seq == 0), self.seq, self.id.len(), self.id.clone())
    }

    /// Cuts everything a network could make arbitrarily large to its limit. The store does this
    /// to every message on its way in, so no adapter has to remember to.
    pub fn bound(&mut self) {
        for attachment in &mut self.attachments {
            attachment.waveform.truncate(WAVEFORM_LIMIT);
        }
        if let Some(thread) = &mut self.thread {
            thread.participants.truncate(THREAD_FACES);
        }
        for reaction in &mut self.reactions {
            reaction.who.truncate(REACTION_REACTORS);
            for id in &mut reaction.who {
                clip(id, 200);
            }
        }
        if let Some(forwarded) = &mut self.forwarded {
            clip(&mut forwarded.from_title, 200);
            clip(&mut forwarded.from_space, 200);
            clip(&mut forwarded.link, 2_000);
            clip(&mut forwarded.author, 200);
            clip(&mut forwarded.text, 4_000);
        }
        if let Some(prior) = &mut self.prior_text {
            clip(prior, PRIOR_LIMIT);
        }
        if let Some(card) = &mut self.card {
            card.bound();
        }
        self.cards.truncate(20);
        for card in &mut self.cards {
            card.bound();
        }
        if let Some(poll) = &mut self.poll {
            clip(&mut poll.question, 100);
            poll.choices.truncate(POLL_OPTIONS);
            for choice in &mut poll.choices {
                clip(&mut choice.text, 100);
                choice.voters.truncate(POLL_VOTERS);
            }
            poll.mine.truncate(POLL_OPTIONS);
            poll.ballots.truncate(1_024);
            for ballot in &mut poll.ballots {
                ballot.choices.truncate(POLL_OPTIONS);
            }
        }
        self.deliveries.truncate(DELIVERIES_LIMIT);
        for delivery in &mut self.deliveries {
            clip(&mut delivery.person, 80);
            clip(&mut delivery.name, 200);
            clip(&mut delivery.failed, 200);
        }
        clip(&mut self.receipt_note, 200);
        if let Some(call) = &mut self.call {
            call.participants.truncate(32);
            for id in &mut call.participants {
                clip(id, 80);
            }
        }
        if let Some(effect) = &mut self.effect {
            clip(effect, 64);
        }
        if let Some(via) = &mut self.sent_via {
            clip(via, 64);
        }
        self.controls.truncate(MESSAGE_CONTROLS_LIMIT);
        for control in &mut self.controls {
            clip(&mut control.block_id, 255);
            clip(&mut control.action_id, 255);
            clip(&mut control.element_type, 64);
            clip(&mut control.label, 256);
            clip(&mut control.value, 2_000);
            if let Some(confirm) = &mut control.confirm {
                clip(&mut confirm.title, 200);
                clip(&mut confirm.text, 1_000);
                clip(&mut confirm.confirm, 100);
                clip(&mut confirm.deny, 100);
            }
            match &mut control.kind {
                ControlKind::Button { url, .. } => clip(url, 2_000),
                ControlKind::Select { placeholder, options, selected, .. } => {
                    clip(placeholder, 200);
                    options.truncate(CONTROL_OPTIONS_LIMIT);
                    for opt in options {
                        clip(&mut opt.label, 200);
                        clip(&mut opt.value, 2_000);
                    }
                    if let Some(sel) = selected {
                        clip(sel, 2_000);
                    }
                }
                ControlKind::DatePicker { placeholder, initial } | ControlKind::TimePicker { placeholder, initial } => {
                    clip(placeholder, 200);
                    if let Some(val) = initial {
                        clip(val, 64);
                    }
                }
                ControlKind::Checkboxes { options, selected } => {
                    options.truncate(CONTROL_OPTIONS_LIMIT);
                    for opt in options {
                        clip(&mut opt.label, 200);
                        clip(&mut opt.value, 2_000);
                    }
                    selected.truncate(CONTROL_OPTIONS_LIMIT);
                    for val in selected {
                        clip(val, 2_000);
                    }
                }
                ControlKind::RadioButtons { options, selected } => {
                    options.truncate(CONTROL_OPTIONS_LIMIT);
                    for opt in options {
                        clip(&mut opt.label, 200);
                        clip(&mut opt.value, 2_000);
                    }
                    if let Some(sel) = selected {
                        clip(sel, 2_000);
                    }
                }
            }
        }
        self.buttons.truncate(MESSAGE_BUTTON_ROWS_LIMIT);
        for row in &mut self.buttons {
            row.truncate(MESSAGE_BUTTONS_PER_ROW_LIMIT);
            for btn in row {
                clip(&mut btn.label, 128);
                match &mut btn.kind {
                    InlineButtonKind::Callback { data, .. } => clip(data, 2_000),
                    InlineButtonKind::Url { url } => clip(url, 2_000),
                    InlineButtonKind::Copy { text } => clip(text, 2_000),
                    InlineButtonKind::SwitchInline { query, .. } => clip(query, 2_000),
                    InlineButtonKind::Unsupported => {}
                }
            }
        }
    }

    /// What is left of a message once it is deleted but must stay: who and when, nothing else.
    /// Iterates over every card on this message (`card` first, then `cards`).
    pub fn all_cards(&self) -> impl Iterator<Item = &Card> {
        self.card.as_deref().into_iter().chain(self.cards.iter())
    }

    pub fn tombstone(&mut self) {
        self.text.clear();
        self.spans.clear();
        self.attachments.clear();
        self.previews.clear();
        self.reactions.clear();
        self.reply_to = None;
        self.forwarded = None;
        self.card = None;
        self.cards.clear();
        self.controls.clear();
        self.buttons.clear();
        self.poll = None;
        self.deliveries.clear();
        self.receipt_note.clear();
        self.pinned = false;
        self.mentions_me = false;
        self.deleted = true;
    }

    /// Every file the message refers to, for registering and for fetching what is on screen.
    pub fn files(&self) -> impl Iterator<Item = &MediaFile> {
        let attached = self
            .attachments
            .iter()
            .flat_map(|attachment| std::iter::once(&attachment.file).chain(&attachment.thumb).chain(&attachment.preview));
        let emoji = self.spans.iter().filter_map(|span| match &span.style {
            Style::Emoji { file } => Some(file),
            _ => None,
        });
        let cards = self
            .all_cards()
            .flat_map(|card| card.image.as_ref().into_iter().chain(card.thumbnail.as_ref()));
        attached
            .chain(self.previews.iter().filter_map(|preview| preview.image.as_ref()))
            .chain(cards)
            .chain(self.reactions.iter().filter_map(|reaction| reaction.file.as_ref()))
            .chain(emoji)
            .chain(self.reply_to.as_ref().and_then(|reply| reply.thumb.as_ref()))
            .chain(&self.sender.avatar)
    }

    /// The same, to fill in paths.
    pub fn files_mut(&mut self) -> impl Iterator<Item = &mut MediaFile> {
        let attached = self
            .attachments
            .iter_mut()
            .flat_map(|attachment| std::iter::once(&mut attachment.file).chain(&mut attachment.thumb).chain(&mut attachment.preview));
        let emoji = self.spans.iter_mut().filter_map(|span| match &mut span.style {
            Style::Emoji { file } => Some(file),
            _ => None,
        });
        let cards = self
            .card
            .as_deref_mut()
            .into_iter()
            .chain(self.cards.iter_mut())
            .flat_map(|card| card.image.as_mut().into_iter().chain(card.thumbnail.as_mut()));
        attached
            .chain(self.previews.iter_mut().filter_map(|preview| preview.image.as_mut()))
            .chain(cards)
            .chain(self.reactions.iter_mut().filter_map(|reaction| reaction.file.as_mut()))
            .chain(emoji)
            .chain(self.reply_to.as_mut().and_then(|reply| reply.thumb.as_mut()))
            .chain(&mut self.sender.avatar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An older frame without `who` still reads, an empty list stays off the wire, and `bound`
    /// holds the list to its cap.
    #[test]
    fn reaction_reactors_read_and_stay_bounded() {
        let plain = Reaction { emoji: "👍".into(), file: None, count: 1, mine: false, who: Vec::new() };
        let on = serde_json::to_string(&plain).unwrap();
        assert!(!on.contains("who"), "an empty list stays off the wire");
        assert_eq!(serde_json::from_str::<Reaction>(&on).unwrap(), plain);

        let older = r#"{"emoji":"👍","count":2,"mine":false}"#;
        assert_eq!(serde_json::from_str::<Reaction>(older).unwrap().who, Vec::<String>::new());

        let mut message = Message::plain("m1", "Ada", "hi", 1_700_000_000, false);
        let many: Vec<String> = (0..REACTION_REACTORS - 1).map(|one| format!("u{one}")).collect();
        let mut named = Reaction { emoji: "👍".into(), file: None, count: 60, mine: false, who: many };
        named.who.insert(1, "u".repeat(500));
        message.reactions.push(named);
        message.bound();
        assert_eq!(message.reactions[0].who.len(), REACTION_REACTORS);
        assert_eq!(message.reactions[0].who[1].len(), 200, "a long id is clipped");
    }

    #[test]
    fn poll_votes_and_round_trip() {
        let mut poll = Poll::open("Lunch?", ["Thai", "Pizza", "Tacos"], false);
        poll.closes_at = Some(1_800_000_000);
        assert_eq!(poll.total_votes(), 0);
        assert_eq!(poll.next_votes(1), vec![1]);

        poll.apply_vote("me", vec![1], "me");
        assert_eq!(poll.mine, vec![1]);
        assert_eq!(poll.choices[1].count, 1);
        assert_eq!(poll.total_votes(), 1);
        // Clicking the same single-choice option retracts it.
        assert_eq!(poll.next_votes(1), Vec::<u32>::new());
        // Clicking another option switches to it.
        assert_eq!(poll.next_votes(2), vec![2]);

        poll.apply_vote("other", vec![0], "me");
        poll.apply_vote("me", vec![2], "me");
        assert_eq!(poll.choices[0].count, 1);
        assert_eq!(poll.choices[1].count, 0);
        assert_eq!(poll.choices[2].count, 1);
        assert_eq!(poll.total_votes(), 2);

        // Multi-choice toggles independently.
        let mut multi = Poll::open("Days?", ["Mon", "Tue", "Wed"], true);
        multi.apply_vote("me", vec![0, 2], "me");
        assert_eq!(multi.next_votes(1), vec![0, 1, 2]);
        assert_eq!(multi.next_votes(0), vec![2]);

        let json = serde_json::to_string(&poll).unwrap();
        assert_eq!(serde_json::from_str::<Poll>(&json).unwrap(), poll);
        // Older wire payload without closes_at/anonymous/total still deserializes.
        let older = r#"{"question":"Q?","choices":[{"text":"A","count":1}]}"#;
        let parsed: Poll = serde_json::from_str(older).unwrap();
        assert_eq!(parsed.closes_at, None);
        assert!(!parsed.anonymous);
        assert_eq!(parsed.total_votes(), 1);
    }

    #[test]
    fn send_options_round_trip_and_stay_off_wire_by_default() {
        let plain = crate::Outgoing::default();
        assert!(plain.is_plain());
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("importance") && !json.contains("broadcast") && !json.contains("effect"));

        let rich = crate::Outgoing {
            importance: crate::Importance::Urgent,
            broadcast: true,
            effect: Some("confetti".into()),
            files: vec![crate::OutgoingFile {
                path: "/tmp/a.jpg".into(),
                view_once: true,
                ..crate::OutgoingFile::default()
            }],
            ..crate::Outgoing::default()
        };
        assert!(!rich.is_plain());
        let wire = serde_json::to_string(&rich).unwrap();
        assert_eq!(serde_json::from_str::<crate::Outgoing>(&wire).unwrap(), rich);
    }

    #[test]
    fn message_controls_round_trip_and_stay_bounded() {
        let mut msg = Message::plain("m1", "Bot", "Deploy?", 1_700_000_000, false);
        let plain_wire = serde_json::to_string(&msg).unwrap();
        assert!(!plain_wire.contains("controls"), "empty controls stay off the wire");

        for i in 0..(MESSAGE_CONTROLS_LIMIT + 5) {
            msg.controls.push(MessageControl {
                block_id: format!("b{i}"),
                action_id: format!("a{i}"),
                element_type: "button".into(),
                label: "X".repeat(300),
                value: "ok".into(),
                confirm: Some(ConfirmDialog {
                    title: "Are you sure?".into(),
                    text: "This ships to prod.".into(),
                    confirm: "Ship".into(),
                    deny: "Cancel".into(),
                }),
                kind: ControlKind::Button { url: String::new(), style: ControlStyle::Primary },
            });
        }
        msg.bound();
        assert_eq!(msg.controls.len(), MESSAGE_CONTROLS_LIMIT);
        assert_eq!(msg.controls[0].label.len(), 256);

        let wire = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.controls, msg.controls);

        msg.tombstone();
        assert!(msg.controls.is_empty());
    }

    #[test]
    fn card_actions_round_trip_and_stay_bounded() {
        let mut msg = Message::plain("m2", "Bot", "", 1_700_000_000, false);
        msg.card = Some(Box::new(Card {
            title: "Build 512".into(),
            actions: (0..20)
                .map(|i| CardAction { label: format!("Action {i} {}", "x".repeat(200)), url: format!("https://example.com/{i}") })
                .collect(),
            ..Card::default()
        }));
        msg.bound();
        let card = msg.card.as_ref().unwrap();
        assert_eq!(card.actions.len(), CARD_ACTIONS);
        assert_eq!(card.actions[0].label.chars().count(), 120);
        let wire = serde_json::to_string(card).unwrap();
        assert_eq!(serde_json::from_str::<Card>(&wire).unwrap(), **card);
        // Older payloads without `actions` still deserialize.
        let legacy: Card = serde_json::from_str(r#"{"title":"Old"}"#).unwrap();
        assert!(legacy.actions.is_empty());
    }

    #[test]
    fn channel_subject_and_announcement_round_trip() {
        let plain = crate::Outgoing::default();
        assert!(!serde_json::to_string(&plain).unwrap().contains("subject"));
        let with_subject = crate::Outgoing { subject: Some("Q4 kickoff".into()), ..crate::Outgoing::default() };
        assert!(!with_subject.is_plain());
        let wire = serde_json::to_string(&with_subject).unwrap();
        assert_eq!(serde_json::from_str::<crate::Outgoing>(&wire).unwrap(), with_subject);

        let mut msg = Message::plain("m3", "Ana", "Launch day", 1_700_000_000, false);
        assert!(!serde_json::to_string(&msg).unwrap().contains("announcement"));
        msg.announcement = true;
        let wire = serde_json::to_string(&msg).unwrap();
        assert!(serde_json::from_str::<Message>(&wire).unwrap().announcement);
    }

    #[test]
    fn reply_to_thumb_is_backward_compatible_and_in_files() {
        let legacy = r#"{"id":"1","sender":"Grace","snippet":"Photo"}"#;
        let parsed: ReplyTo = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.thumb, None);
        assert!(!serde_json::to_string(&parsed).unwrap().contains("thumb"));

        let mut msg = Message::plain("m4", "Ada", "Nice shot!", 1_700_000_000, false);
        msg.reply_to = Some(ReplyTo {
            id: "1".into(),
            sender: "Grace".into(),
            snippet: "Photo".into(),
            person: None,
            deleted: false,
            thumb: Some(MediaFile::new("signal:quote:1", MediaKind::Image)),
            story: false,
        });
        assert_eq!(msg.files().count(), 1);
        assert_eq!(msg.files().next().unwrap().id, "signal:quote:1");
        let wire = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<Message>(&wire).unwrap().reply_to, msg.reply_to);

        // Story reply flag is backward-compatible and omitted when false.
        assert!(!parsed.story);
        assert!(!serde_json::to_string(&parsed).unwrap().contains("story"));
        let mut story_reply = parsed;
        story_reply.story = true;
        let story_wire = serde_json::to_string(&story_reply).unwrap();
        assert!(story_wire.contains(r#""story":true"#));
        assert!(serde_json::from_str::<ReplyTo>(&story_wire).unwrap().story);
    }

    #[test]
    fn inline_buttons_round_trip_and_stay_bounded() {
        let mut msg = Message::plain("m4", "Bot", "Pick one", 1_700_000_000, false);
        assert!(!serde_json::to_string(&msg).unwrap().contains("buttons"));
        msg.buttons = vec![vec![
            InlineButton {
                label: "Approve".into(),
                kind: InlineButtonKind::Callback { data: "617070".into(), requires_password: false },
            },
            InlineButton {
                label: "Docs".into(),
                kind: InlineButtonKind::Url { url: "https://example.com".into() },
            },
        ]];
        msg.bound();
        let wire = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.buttons, msg.buttons);
        msg.tombstone();
        assert!(msg.buttons.is_empty());
    }
}
