//! Everything chatd remembers lives here, on disk. Nothing is cached in the process.
//!
//! The database is a cache of what the networks hold. Sessions are in the keychain, so a schema
//! change drops every table and the adapters fill them again.
//!
//! Every table is keyed by `(network, account)`. Empty text stands for "none" in the columns that
//! name another row (`space`, `section`, `avatar`, `peer`, `last_read`).

use protocol::{
    clip, AccountKey, CallSupport, Conversation, ConversationKind, Folder, Group, InboxCounts,
    InboxFilter, Key, Mark, MediaFile, MediaKind, Message, Network, Page, Person, Presence,
    Reaction, Relation, SendState, SharedItem, SharedKind, Shelf, Space, Special, DRAFT_LIMIT,
    FOLDERS_LIMIT, FOLDERS_PER_ROW, FOLDER_MEMBERS,
};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, Transaction};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::Path;

/// Raise this when a table changes. `open` then drops the cache and starts over.
const SCHEMA: i32 = 4;

/// Retention: the newest messages kept per conversation.
pub const KEEP_MESSAGES: u32 = 2_000;
/// Retention: older messages go, unless the conversation was opened within this many days. The
/// default of Settings › Privacy › Keep messages (P4), which may shorten it.
pub const KEEP_DAYS: i64 = 90;
/// Most sizes one file is registered in.
pub const VARIANTS: usize = 6;
/// Most characters of a conversation's topic kept.
const TOPIC: usize = 300;
/// The Inbox keeps a channel this long after it mentioned me, read or not.
pub const NOTICED_SECONDS: i64 = 86_400;
/// Idle this long, a row leaves the Inbox (unless starred, unread, drafted or failed) and is
/// `quiet` in a tree.
pub const QUIET_SECONDS: i64 = 30 * 86_400;
/// "Talked to lately", for People's Recent section.
pub const RECENT_SECONDS: i64 = 14 * 86_400;


const TABLES: &str = "
    CREATE TABLE account (
        network TEXT NOT NULL,
        id      TEXT NOT NULL,
        name    TEXT NOT NULL DEFAULT '',
        me      TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (network, id)
    ) WITHOUT ROWID;
    CREATE TABLE space (
        network  TEXT NOT NULL,
        account  TEXT NOT NULL,
        id       TEXT NOT NULL,
        name     TEXT NOT NULL,
        icon     TEXT NOT NULL DEFAULT '',
        position INTEGER NOT NULL DEFAULT 0,
        muted    INTEGER NOT NULL DEFAULT 0,
        hide_muted INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (network, account, id)
    ) WITHOUT ROWID;
    -- A folder: of chats (Telegram, membership on the conversation) or of Spaces (Discord,
    -- `spaces` as ',a,b,' in order). `pinned` is the folder's own pinned chats, in order.
    CREATE TABLE folder (
        network  TEXT NOT NULL,
        account  TEXT NOT NULL,
        id       TEXT NOT NULL,
        name     TEXT NOT NULL DEFAULT '',
        position INTEGER NOT NULL DEFAULT 0,
        color    INTEGER,
        spaces   TEXT NOT NULL DEFAULT '',
        pinned   TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (network, account, id)
    ) WITHOUT ROWID;
    -- A Group of the protocol: a category or sidebar section of one Space.
    CREATE TABLE section (
        network   TEXT NOT NULL,
        account   TEXT NOT NULL,
        space     TEXT NOT NULL,
        id        TEXT NOT NULL,
        name      TEXT NOT NULL,
        position  INTEGER NOT NULL DEFAULT 0,
        collapsed INTEGER NOT NULL DEFAULT 0,
        kind      TEXT NOT NULL DEFAULT 'section',
        muted     INTEGER NOT NULL DEFAULT 0,
        unread_only INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (network, account, space, id)
    ) WITHOUT ROWID;
    CREATE TABLE conversation (
        network   TEXT NOT NULL,
        account   TEXT NOT NULL,
        id        TEXT NOT NULL,
        kind      TEXT NOT NULL,
        title     TEXT NOT NULL,
        space     TEXT NOT NULL DEFAULT '',
        section   TEXT NOT NULL DEFAULT '',
        position  INTEGER NOT NULL DEFAULT 0,
        avatar    TEXT NOT NULL DEFAULT '',
        peer      TEXT NOT NULL DEFAULT '',
        call      TEXT NOT NULL DEFAULT 'none',
        -- The adapter wants it in the Inbox whatever its kind.
        listed    INTEGER NOT NULL DEFAULT 0,
        unread    INTEGER NOT NULL DEFAULT 0,
        mentions  INTEGER NOT NULL DEFAULT 0,
        muted     INTEGER NOT NULL DEFAULT 0,
        starred   INTEGER NOT NULL DEFAULT 0,
        -- The read marker: the newest message read here or on another device.
        last_read TEXT NOT NULL DEFAULT '',
        preview   TEXT NOT NULL DEFAULT '',
        updated   INTEGER NOT NULL DEFAULT 0,
        -- When the user last opened it, for retention.
        opened    INTEGER NOT NULL DEFAULT 0,
        -- Set while a Space's tree is being replaced; rows still set afterwards are gone.
        stale     INTEGER NOT NULL DEFAULT 0,
        topic     TEXT NOT NULL DEFAULT '',
        -- A thread's parent conversation and the message there that started it.
        parent    TEXT NOT NULL DEFAULT '',
        root      TEXT NOT NULL DEFAULT '',
        read_only INTEGER NOT NULL DEFAULT 0,
        private   INTEGER NOT NULL DEFAULT 0,
        -- What the user typed and did not send. Local.
        draft     TEXT NOT NULL DEFAULT '',
        -- How the network files it: folder ids as ',a,b,', archived, its pin order (NULL: not
        -- pinned), hidden, a followed thread. Written only when the adapter says (`Filing`).
        folders   TEXT NOT NULL DEFAULT '',
        archived  INTEGER NOT NULL DEFAULT 0,
        pin       INTEGER,
        hidden    INTEGER NOT NULL DEFAULT 0,
        followed  INTEGER NOT NULL DEFAULT 0,
        bot       INTEGER NOT NULL DEFAULT 0,
        meeting   INTEGER NOT NULL DEFAULT 0,
        special   TEXT NOT NULL DEFAULT 'none',
        mark      TEXT NOT NULL DEFAULT '',
        opted_out INTEGER NOT NULL DEFAULT 0,
        -- When it last mentioned me: the Inbox keeps a channel a day after that.
        noticed   INTEGER NOT NULL DEFAULT 0,
        -- The newest message's sender, whether it was mine, and its id.
        preview_from TEXT NOT NULL DEFAULT '',
        preview_mine INTEGER NOT NULL DEFAULT 0,
        preview_id   TEXT NOT NULL DEFAULT '',
        -- The newest of my messages the other side has read.
        seen_to   TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (network, account, id)
    ) WITHOUT ROWID;
    CREATE INDEX conversation_threads ON conversation (network, account, parent) WHERE parent != '';
    CREATE INDEX conversation_recent ON conversation (updated DESC);
    CREATE INDEX conversation_tree ON conversation (network, account, space, position);
    CREATE TABLE message (
        network      TEXT NOT NULL,
        account      TEXT NOT NULL,
        conversation TEXT NOT NULL,
        id           TEXT NOT NULL,
        sent         INTEGER NOT NULL,
        text         TEXT NOT NULL,
        -- 0 on the network, 1 pending, 2 failed.
        pending      INTEGER NOT NULL DEFAULT 0,
        client_id    TEXT,
        -- The rest of the message as the protocol's JSON: sender, ranges, attachments, link
        -- previews, reactions, reply, thread summary, flags.
        body         TEXT NOT NULL,
        PRIMARY KEY (network, account, conversation, id)
    ) WITHOUT ROWID;
    CREATE INDEX message_by_time ON message (network, account, conversation, sent);
    CREATE INDEX message_unsent ON message (pending) WHERE pending != 0;
    CREATE TABLE person (
        network  TEXT NOT NULL,
        account  TEXT NOT NULL,
        id       TEXT NOT NULL,
        name     TEXT NOT NULL,
        handle   TEXT NOT NULL DEFAULT '',
        avatar   TEXT NOT NULL DEFAULT '',
        bot      INTEGER NOT NULL DEFAULT 0,
        status   TEXT NOT NULL DEFAULT '',
        -- The relationship: friend, pending either way, blocked, contact. 'none' is merely seen.
        relation TEXT NOT NULL DEFAULT 'none',
        -- Opens the person in the network's own app.
        link     TEXT NOT NULL DEFAULT '',
        title    TEXT NOT NULL DEFAULT '',
        utc_offset INTEGER,
        mark     TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (network, account, id)
    ) WITHOUT ROWID;
    CREATE INDEX person_by_name ON person (name COLLATE NOCASE);
    -- A file the network can serve. `path` is set once it is on disk.
    CREATE TABLE media (
        network TEXT NOT NULL,
        account TEXT NOT NULL,
        id      TEXT NOT NULL,
        kind    TEXT NOT NULL,
        width   INTEGER NOT NULL DEFAULT 0,
        height  INTEGER NOT NULL DEFAULT 0,
        bytes   INTEGER NOT NULL DEFAULT 0,
        url     TEXT NOT NULL DEFAULT '',
        path    TEXT,
        used    INTEGER NOT NULL DEFAULT 0,
        -- Why the last fetch failed. Nothing fetches it again while this is set.
        failed  TEXT,
        -- The longest side, in pixels, of the variant `url` names. 0: the address the file was
        -- registered with, whose size is its `width` and `height`.
        size    INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (network, account, id)
    ) WITHOUT ROWID;
    CREATE INDEX media_on_disk ON media (used) WHERE path IS NOT NULL;
    -- The same picture at other sizes, for a screen that draws it larger than it was registered.
    CREATE TABLE media_variant (
        network TEXT NOT NULL,
        account TEXT NOT NULL,
        id      TEXT NOT NULL,
        size    INTEGER NOT NULL,
        url     TEXT NOT NULL,
        PRIMARY KEY (network, account, id, size)
    ) WITHOUT ROWID;
    -- Whatever an adapter looked up once and should not ask for again.
    CREATE TABLE name (
        network TEXT NOT NULL,
        account TEXT NOT NULL,
        id      TEXT NOT NULL,
        value   TEXT NOT NULL,
        PRIMARY KEY (network, account, id)
    ) WITHOUT ROWID;";

/// The number a row shows (`Conversation::badge`), as SQL over `conversation c`. A bot, an
/// archived chat count only mentions; a request and a hidden row, which are never drawn
/// where the number is, count nothing.
const ATTENTION: &str = "CASE WHEN c.special = 'request' OR c.hidden THEN 0
                              WHEN c.kind IN ('direct', 'group') AND NOT c.muted AND NOT c.bot AND NOT c.archived
                              THEN max(c.unread, c.mentions) ELSE c.mentions END";

const CONVERSATION: &str = "
    SELECT c.network, c.account, c.id, c.kind, c.title, c.space, c.section, c.position, c.peer,
           c.unread, c.mentions, c.muted, c.starred, c.preview, c.updated, c.last_read, c.call,
           m.id, m.kind, m.width, m.height, m.bytes, m.path, m.failed,
           COALESCE(s.name, ''), COALESCE(g.name, ''), c.topic, c.parent, c.root,
           c.read_only, c.private, c.draft,
           c.folders, c.archived, c.pin, c.hidden, c.followed, c.bot, c.meeting, c.special, c.mark,
           c.opted_out, c.preview_from, c.preview_mine,
           (c.preview_mine AND c.seen_to != '' AND (length(c.seen_to), c.seen_to) >= (length(c.preview_id), c.preview_id)),
           (c.updated < CAST(strftime('%s', 'now') AS INTEGER) - 2592000 AND c.unread = 0 AND c.mentions = 0
            AND NOT c.starred AND c.draft = '')
    FROM conversation c
    LEFT JOIN media m ON m.network = c.network AND m.account = c.account AND m.id = c.avatar
    LEFT JOIN space s ON s.network = c.network AND s.account = c.account AND s.id = c.space
    LEFT JOIN section g ON g.network = c.network AND g.account = c.account AND g.space = c.space
                       AND g.id = c.section";

/// Which rows the Inbox holds at all, as SQL over `conversation c` (`Request::Inbox` says it in
/// words).
const IN_INBOX: &str = "(NOT c.hidden AND c.special != 'request'
     AND (c.kind IN ('direct', 'group') OR c.starred OR c.listed OR c.mentions > 0 OR c.noticed >= CAST(strftime('%s', 'now') AS INTEGER) - 86400)
     AND (c.updated > 0 OR c.starred OR c.draft != '')
     AND (NOT c.archived OR c.mentions > 0 OR c.starred)
     AND (c.updated >= CAST(strftime('%s', 'now') AS INTEGER) - 2592000 OR c.starred OR c.unread > 0 OR c.mentions > 0 OR c.draft != ''
          OR EXISTS (SELECT 1 FROM message f WHERE f.network = c.network AND f.account = c.account
                        AND f.conversation = c.id AND f.pending = 2)))";
/// The Inbox filters. Unread and Mentions are different questions: what have I not seen, and
/// who is waiting on me. A bot's or an archived chat's plain unread is neither.
const IS_UNREAD: &str = "(((c.unread > 0 AND NOT c.bot AND NOT c.archived) OR c.mentions > 0) AND NOT c.muted)";
const IS_MENTION: &str = "(c.mentions > 0)";
const IS_APP: &str = "(c.bot AND c.unread > 0)";

const PERSON: &str = "
    SELECT p.network, p.account, p.id, p.name, p.handle, p.bot, p.relation, p.status, p.link,
           m.id, m.kind, m.width, m.height, m.bytes, m.path, m.failed,
           p.title, p.utc_offset, p.mark
    FROM person p
    LEFT JOIN media m ON m.network = p.network AND m.account = p.account AND m.id = p.avatar";

/// Messages in the order they are read: by time, then by id, shorter ids first so numeric ids of
/// different lengths still sort as numbers.
const NEWEST_FIRST: &str = "ORDER BY sent DESC, length(id) DESC, id DESC";
const OLDEST_FIRST: &str = "ORDER BY sent, length(id), id";

pub struct Store {
    db: Connection,
}

/// What an adapter knows about a conversation. Unread state, the preview and what the user did
/// locally (starred) are not here: writing a row never disturbs them.
#[derive(Debug, Clone)]
pub struct ConversationRow {
    pub id: String,
    pub kind: ConversationKind,
    pub title: String,
    pub space: Option<String>,
    pub group: Option<String>,
    /// Orders the whole Space from top to bottom, groups included: a conversation comes after
    /// every row with a lower position whatever group either is in.
    pub position: i32,
    /// The id of a registered [`MediaFile`].
    pub avatar: Option<String>,
    pub peer: Option<String>,
    pub call: CallSupport,
    /// Show it in the Inbox even though it is a channel nobody mentioned me in.
    pub listed: bool,
    /// One line on what it is for. Cut to 300 characters.
    pub topic: String,
    /// For a `Thread`: the conversation it hangs off and the message there that started it. A
    /// thread that names its parent stays through a tree replacement and goes when its parent
    /// goes, or when its first message has aged out and nobody opened it since.
    pub parent: Option<String>,
    pub root: Option<String>,
    /// We may read it but not post.
    pub read_only: bool,
    pub private: bool,
    /// The other side is a bot or app.
    pub bot: bool,
    /// A meeting's chat.
    pub meeting: bool,
    pub special: Special,
    pub mark: Option<Mark>,
    /// Not opted in to (Discord's "show all channels" off).
    pub opted_out: bool,
    /// How the network files it. `None` leaves what is stored, so a tree that does not know
    /// (a Slack tree reload) never undoes a pin or a follow; `Some` replaces it.
    pub filing: Option<Filing>,
}

/// Where the network files a conversation: folders, archive, pin, hidden, followed. Written
/// with the row (`ConversationRow::filing`) or alone (`Context::filing`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filing {
    /// Folder ids, at most 32.
    pub folders: Vec<String>,
    pub archived: bool,
    /// Its place among the network's pins, lowest first. `None`: not pinned.
    pub pin: Option<i32>,
    pub hidden: bool,
    pub followed: bool,
}

/// One folder as an adapter reports it. chatd counts what is in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderRow {
    pub id: String,
    pub name: String,
    pub position: i32,
    pub color: Option<u32>,
    /// The Spaces it holds, in order (Discord server folders).
    pub spaces: Vec<String>,
    /// Its own pinned chats, in order (Telegram).
    pub pinned: Vec<String>,
}

/// What a person's row says beyond `PersonRow`, as the network tells it: kept with the person.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonDetails {
    pub title: String,
    pub utc_offset: Option<i32>,
    pub mark: Option<Mark>,
}

/// The newest message as a line, for a network whose list says only that (Teams' chat list).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewLine {
    pub id: String,
    pub text: String,
    /// Who said it. Ignored when `mine`.
    pub sender: String,
    pub mine: bool,
    /// Unix seconds.
    pub sent: i64,
}

/// As stored: folder and Space ids never hold the comma that separates them.
fn id_list(ids: &[String], limit: usize) -> String {
    let kept: Vec<String> = ids.iter().take(limit).map(|id| id.replace(',', "")).filter(|id| !id.is_empty()).collect();
    if kept.is_empty() {
        return String::new();
    }
    format!(",{},", kept.join(","))
}

fn from_id_list(stored: &str) -> Vec<String> {
    stored.split(',').filter(|id| !id.is_empty()).map(str::to_owned).collect()
}

impl ConversationRow {
    pub fn new(id: impl Into<String>, kind: ConversationKind, title: impl Into<String>) -> Self {
        ConversationRow {
            id: id.into(),
            kind,
            title: title.into(),
            space: None,
            group: None,
            position: 0,
            avatar: None,
            peer: None,
            call: CallSupport::None,
            listed: false,
            topic: String::new(),
            parent: None,
            root: None,
            read_only: false,
            private: false,
            bot: false,
            meeting: false,
            special: Special::None,
            mark: None,
            opted_out: false,
            filing: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpaceRow {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub position: i32,
}

#[derive(Debug, Clone, Default)]
pub struct PersonRow {
    pub id: String,
    pub name: String,
    pub handle: String,
    pub avatar: Option<String>,
    pub bot: bool,
    pub status: String,
    /// Opens the person in the network's own app. Empty when the network has no such link.
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    pub key: AccountKey,
    pub name: String,
    pub me: String,
    pub attention: u32,
    pub unread: bool,
    /// Our own picture, once the adapter recorded us as a person.
    pub avatar: Option<MediaFile>,
}

/// What `ingest` did with a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    New,
    /// It was there and is different now: an edit, a reaction, a link preview arriving.
    Updated,
    /// It was there, identical. Nothing to tell the shell.
    Same,
}

/// A Space's groups and one page of its conversations.
pub struct Tree {
    pub groups: Vec<Group>,
    pub conversations: Vec<Conversation>,
    /// How many conversations the Space holds in all.
    pub total: u32,
}

pub struct MessagePage {
    pub messages: Vec<Message>,
    pub last_read: Option<String>,
    pub older: bool,
    pub newer: bool,
}

/// A file on disk, for the media cache's eviction.
pub struct OnDisk {
    pub account: AccountKey,
    pub id: String,
    pub path: String,
    pub bytes: u64,
}

/// How an enum is spelled on the wire, which is also how it is stored.
fn word<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(word)) => word,
        _ => String::new(),
    }
}

fn from_word<T: DeserializeOwned>(word: String) -> rusqlite::Result<T> {
    serde_json::from_value(serde_json::Value::String(word))
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

fn broken(err: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(err))
}

fn some(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn text(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("")
}

/// `%` and `_` mean themselves in what the user typed.
fn like_prefix(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 1);
    for c in query.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

/// The seven `media` columns of a left join, starting at `at`.
fn file_at(row: &Row, at: usize) -> rusqlite::Result<Option<MediaFile>> {
    let Some(id) = row.get::<_, Option<String>>(at)? else {
        return Ok(None);
    };
    Ok(Some(MediaFile {
        id,
        kind: from_word(row.get(at + 1)?)?,
        width: row.get(at + 2)?,
        height: row.get(at + 3)?,
        bytes: row.get::<_, i64>(at + 4)? as u64,
        path: row.get(at + 5)?,
        failed: row.get(at + 6)?,
    }))
}

fn key_at(row: &Row) -> rusqlite::Result<Key> {
    Ok(Key {
        network: from_word(row.get(0)?)?,
        account: row.get(1)?,
        id: row.get(2)?,
    })
}

fn conversation_from(row: &Row) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        key: key_at(row)?,
        kind: from_word(row.get(3)?)?,
        title: row.get(4)?,
        space: some(row.get(5)?),
        group: some(row.get(6)?),
        position: row.get(7)?,
        peer: some(row.get(8)?),
        unread: row.get(9)?,
        mentions: row.get(10)?,
        muted: row.get(11)?,
        starred: row.get(12)?,
        preview: row.get(13)?,
        updated: row.get(14)?,
        last_read: some(row.get(15)?),
        call: from_word(row.get(16)?)?,
        avatar: file_at(row, 17)?,
        space_title: row.get(24)?,
        group_title: row.get(25)?,
        topic: row.get(26)?,
        parent: some(row.get(27)?),
        root: some(row.get(28)?),
        read_only: row.get(29)?,
        private: row.get(30)?,
        draft: row.get(31)?,
        occupants: None,
        folders: from_id_list(&row.get::<_, String>(32)?),
        archived: row.get(33)?,
        pin: row.get(34)?,
        hidden: row.get(35)?,
        followed: row.get(36)?,
        bot: row.get(37)?,
        meeting: row.get(38)?,
        special: from_word(row.get(39)?)?,
        mark: mark_from(row.get(40)?)?,
        opted_out: row.get(41)?,
        preview_from: row.get(42)?,
        preview_mine: row.get(43)?,
        seen: row.get(44)?,
        quiet: row.get(45)?,
    })
}

fn counts_from(row: &Row) -> rusqlite::Result<InboxCounts> {
    Ok(InboxCounts {
        all: row.get(0)?,
        unread: row.get(1)?,
        mentions: row.get(2)?,
        apps: row.get(3)?,
    })
}

fn mark_from(word: String) -> rusqlite::Result<Option<Mark>> {
    match word.as_str() {
        "" => Ok(None),
        _ => from_word(word).map(Some),
    }
}

fn mark_word(mark: &Option<Mark>) -> String {
    mark.as_ref().map_or_else(String::new, word)
}

fn person_from(row: &Row) -> rusqlite::Result<Person> {
    Ok(Person {
        key: key_at(row)?,
        name: row.get(3)?,
        handle: row.get(4)?,
        bot: row.get(5)?,
        relation: from_word(row.get(6)?)?,
        status: row.get(7)?,
        link: row.get(8)?,
        presence: Presence::Unknown,
        avatar: file_at(row, 9)?,
        title: row.get(16)?,
        utc_offset: row.get(17)?,
        mark: mark_from(row.get(18)?)?,
        emoji: String::new(),
        activity: None,
    })
}

/// The message as stored: the columns hold what SQL needs, the body holds the rest.
fn message_from(row: &Row) -> rusqlite::Result<Message> {
    let body: String = row.get(2)?;
    let mut message: Message = serde_json::from_str(&body).map_err(broken)?;
    message.id = row.get(0)?;
    message.text = row.get(1)?;
    Ok(message)
}

/// The row's second line: the text, or what was attached, in words.
fn preview_of(message: &Message) -> String {
    if !message.text.is_empty() {
        return message.text.chars().take(120).collect();
    }
    let Some(first) = message.attachments.first() else {
        return match &message.card {
            Some(card) if !card.title.is_empty() => card.title.chars().take(120).collect(),
            _ => String::new(),
        };
    };
    let count = message.attachments.len();
    let all = |kind: MediaKind| message.attachments.iter().all(|attachment| attachment.file.kind == kind);
    let words = |one: &str, many: &str| if count == 1 { one.to_owned() } else { format!("{count} {many}") };
    if first.sticker {
        "Sticker".to_owned()
    } else if !first.waveform.is_empty() {
        "Voice message".to_owned()
    } else if all(MediaKind::Image) {
        words("Photo", "photos")
    } else if all(MediaKind::Video) {
        words("Video", "videos")
    } else if all(MediaKind::Gif) {
        words("GIF", "GIFs")
    } else if count == 1 {
        first.name.chars().take(120).collect()
    } else {
        format!("{count} files")
    }
}

/// Threads go with the conversation they hang off.
fn drop_orphan_threads(tx: &Transaction, account: &AccountKey) -> rusqlite::Result<()> {
    let key = params![word(&account.network), account.account];
    let orphans = "SELECT t.id FROM conversation t
          WHERE t.network = ?1 AND t.account = ?2 AND t.parent != '' AND NOT EXISTS
                (SELECT 1 FROM conversation p
                  WHERE p.network = t.network AND p.account = t.account AND p.id = t.parent)";
    tx.execute(
        &format!("DELETE FROM message WHERE network = ?1 AND account = ?2 AND conversation IN ({orphans})"),
        key,
    )?;
    tx.execute(
        "DELETE FROM conversation AS t
          WHERE t.network = ?1 AND t.account = ?2 AND t.parent != '' AND NOT EXISTS
                (SELECT 1 FROM conversation p
                  WHERE p.network = t.network AND p.account = t.account AND p.id = t.parent)",
        key,
    )?;
    Ok(())
}

fn upsert_conversation(tx: &Transaction, account: &AccountKey, row: &ConversationRow) -> rusqlite::Result<()> {
    let mut topic = row.topic.lines().next().unwrap_or("").to_owned();
    clip(&mut topic, TOPIC);
    let filing = row.filing.clone().unwrap_or_default();
    tx.execute(
        "INSERT INTO conversation
             (network, account, id, kind, title, space, section, position, avatar, peer, call, listed,
              topic, parent, root, read_only, private, bot, meeting, special, mark, opted_out,
              folders, archived, pin, hidden, followed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                 ?20, ?21, ?22, ?24, ?25, ?26, ?27, ?28)
         ON CONFLICT (network, account, id) DO UPDATE SET
             kind = excluded.kind, title = excluded.title, space = excluded.space,
             section = excluded.section, position = excluded.position, avatar = excluded.avatar,
             peer = excluded.peer, call = excluded.call, listed = excluded.listed, stale = 0,
             topic = excluded.topic, parent = excluded.parent, root = excluded.root,
             read_only = excluded.read_only, private = excluded.private, bot = excluded.bot,
             meeting = excluded.meeting, special = excluded.special, mark = excluded.mark,
             opted_out = excluded.opted_out,
             folders = CASE WHEN ?23 THEN excluded.folders ELSE folders END,
             archived = CASE WHEN ?23 THEN excluded.archived ELSE archived END,
             pin = CASE WHEN ?23 THEN excluded.pin ELSE pin END,
             hidden = CASE WHEN ?23 THEN excluded.hidden ELSE hidden END,
             followed = CASE WHEN ?23 THEN excluded.followed ELSE followed END",
        params![
            word(&account.network),
            account.account,
            row.id,
            word(&row.kind),
            row.title,
            text(&row.space),
            text(&row.group),
            row.position,
            text(&row.avatar),
            text(&row.peer),
            word(&row.call),
            row.listed,
            topic,
            text(&row.parent),
            text(&row.root),
            row.read_only,
            row.private,
            row.bot,
            row.meeting,
            word(&row.special),
            mark_word(&row.mark),
            row.opted_out,
            row.filing.is_some(),
            id_list(&filing.folders, FOLDERS_PER_ROW),
            filing.archived,
            filing.pin,
            filing.hidden,
            filing.followed,
        ],
    )?;
    Ok(())
}

/// The user's settings (`protocol::settings`). Not part of the cache: a schema change keeps it,
/// and only sign-out and Reset all delete rows.
const SETTINGS: &str = "
    CREATE TABLE IF NOT EXISTS settings (
        scope TEXT NOT NULL,
        key   TEXT NOT NULL,
        value TEXT NOT NULL,
        PRIMARY KEY (scope, key)
    ) WITHOUT ROWID;
";

/// The shape of the settings rows. A migration from an older one runs in `migrate_settings`.
const SETTINGS_VERSION: u32 = 1;

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Store> {
        let db = Connection::open(path)?;
        // A 1 MB page cache, half SQLite's default: every list read is one page of rows, the
        // file is on a local disk the system already caches, and chatd's idle budget is tight.
        db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA cache_size = -1024;")?;
        let version: i32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA {
            let old: Vec<String> = db
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != 'settings'")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for table in old {
                db.execute_batch(&format!("DROP TABLE \"{table}\";"))?;
            }
            db.execute_batch(TABLES)?;
            db.execute_batch(&format!("PRAGMA user_version = {SCHEMA};"))?;
        }
        let mut store = Store { db };
        store.migrate_settings()?;
        Ok(store)
    }

    // Settings.

    /// Creates the settings table and brings its rows to [`SETTINGS_VERSION`], in one
    /// transaction, before the socket is served. There is nothing to convert yet.
    fn migrate_settings(&mut self) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        tx.execute_batch(SETTINGS)?;
        tx.execute(
            "INSERT OR IGNORE INTO settings (scope, key, value) VALUES ('', ?1, ?2)",
            params![protocol::settings::VERSION, SETTINGS_VERSION.to_string()],
        )?;
        tx.commit()
    }

    /// Every stored setting, the version row included.
    pub fn settings(&self) -> rusqlite::Result<Vec<protocol::settings::Setting>> {
        let mut query = self.db.prepare("SELECT scope, key, value FROM settings ORDER BY scope, key")?;
        let rows = query.query_map([], |row| {
            Ok(protocol::settings::Setting { scope: row.get(0)?, key: row.get(1)?, value: row.get(2)? })
        })?;
        rows.collect()
    }

    /// Stores one value, or deletes the row so the default applies again.
    pub fn set_setting(&mut self, scope: &str, key: &str, value: Option<&str>) -> rusqlite::Result<()> {
        match value {
            Some(value) => self.db.execute(
                "INSERT INTO settings (scope, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT (scope, key) DO UPDATE SET value = excluded.value",
                params![scope, key, value],
            )?,
            None => self.db.execute("DELETE FROM settings WHERE scope = ?1 AND key = ?2", params![scope, key])?,
        };
        self.settle()
    }

    /// Reset all (B3): every row but the version.
    pub fn reset_settings(&mut self) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM settings WHERE NOT (scope = '' AND key = ?1)", params![protocol::settings::VERSION])?;
        self.settle()
    }

    /// Puts a settings write on the disk now. The rest of this file is a cache of what the
    /// networks hold, and may sync only at checkpoints (synchronous = NORMAL); a choice the
    /// user just made is not, and a power cut must not undo it. A checkpoint syncs the log and
    /// the file; settings change a few times a day, so its cost does not matter.
    fn settle(&self) -> rusqlite::Result<()> {
        self.db.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))
    }

    // Storage.

    /// Clear (D3): every message that is not the user's own unsent one, and every file's place
    /// on disk. Conversations, people, drafts and settings stay. Then the file shrinks.
    pub fn clear_cache(&mut self) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM message WHERE pending = 0", [])?;
        tx.execute("UPDATE media SET path = NULL WHERE path IS NOT NULL", [])?;
        tx.commit()?;
        self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
    }

    /// What `prune` with `days` would delete by age now: messages, and about how many bytes
    /// their rows hold.
    pub fn prunable(&self, now: i64, days: u32) -> rusqlite::Result<(u64, u64)> {
        let cutoff = now - i64::from(days) * 86_400;
        self.db.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(text) + LENGTH(body) + 64), 0) FROM message
              WHERE pending = 0 AND sent < ?1 AND NOT EXISTS
                 (SELECT 1 FROM conversation c
                   WHERE c.network = message.network AND c.account = message.account
                     AND c.id = message.conversation AND c.opened >= ?1)",
            params![cutoff],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )
    }

    // Accounts.

    pub fn set_account(&mut self, account: &AccountKey, name: &str, me: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO account (network, id, name, me) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (network, id) DO UPDATE SET
                 name = CASE WHEN excluded.name = '' THEN name ELSE excluded.name END,
                 me = CASE WHEN excluded.me = '' THEN me ELSE excluded.me END",
            params![word(&account.network), account.account, name, me],
        )?;
        Ok(())
    }

    /// Every account with rows here, with the totals its rail button shows.
    pub fn accounts(&self) -> rusqlite::Result<Vec<AccountRow>> {
        let mut query = self.db.prepare(&format!(
            "SELECT a.network, a.id, a.name, a.me,
                    (SELECT COALESCE(SUM({ATTENTION}), 0) FROM conversation c
                      WHERE c.network = a.network AND c.account = a.id),
                    EXISTS (SELECT 1 FROM conversation c
                      WHERE c.network = a.network AND c.account = a.id
                        AND c.unread > 0 AND NOT c.muted AND {ATTENTION} = 0),
                    m.id, m.kind, m.width, m.height, m.bytes, m.path, m.failed
             FROM account a
             LEFT JOIN person p ON p.network = a.network AND p.account = a.id AND p.id = a.me
             LEFT JOIN media m ON m.network = a.network AND m.account = a.id AND m.id = p.avatar
             ORDER BY a.network, a.name, a.id"
        ))?;
        let rows = query.query_map([], |row| {
            Ok(AccountRow {
                key: AccountKey {
                    network: from_word(row.get(0)?)?,
                    account: row.get(1)?,
                },
                name: row.get(2)?,
                me: row.get(3)?,
                attention: row.get(4)?,
                unread: row.get(5)?,
                avatar: file_at(row, 6)?,
            })
        })?;
        rows.collect()
    }

    /// Forgets everything stored for an account, for sign-out. The files themselves are the media
    /// cache's to delete.
    pub fn forget(&mut self, account: &AccountKey) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        let key = params![word(&account.network), account.account];
        tx.execute("DELETE FROM account WHERE network = ?1 AND id = ?2", key)?;
        for table in ["message", "conversation", "section", "space", "folder", "person", "media", "media_variant", "name"] {
            tx.execute(&format!("DELETE FROM {table} WHERE network = ?1 AND account = ?2"), key)?;
        }
        // No trace of a removed account stays in its settings either: its own and its
        // conversations'.
        let own = protocol::settings::account_scope(account);
        let conversations = protocol::settings::conversation_scope(&Key::new(account, ""));
        tx.execute(
            "DELETE FROM settings WHERE scope = ?1 OR substr(scope, 1, length(?2)) = ?2",
            params![own, conversations],
        )?;
        tx.commit()
    }

    // Spaces and their trees.

    /// The account's Spaces, all of them. A Space that is no longer listed goes, with its tree.
    pub fn replace_spaces(&mut self, account: &AccountKey, spaces: &[SpaceRow]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        let (network, id) = (word(&account.network), &account.account);
        let old: Vec<String> = tx
            .prepare("SELECT id FROM space WHERE network = ?1 AND account = ?2")?
            .query_map(params![network, id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for gone in old.iter().filter(|old| !spaces.iter().any(|space| space.id == **old)) {
            let key = params![network, id, gone];
            tx.execute(
                "DELETE FROM message WHERE network = ?1 AND account = ?2 AND conversation IN
                     (SELECT id FROM conversation WHERE network = ?1 AND account = ?2 AND space = ?3)",
                key,
            )?;
            tx.execute("DELETE FROM conversation WHERE network = ?1 AND account = ?2 AND space = ?3", key)?;
            tx.execute("DELETE FROM section WHERE network = ?1 AND account = ?2 AND space = ?3", key)?;
            tx.execute("DELETE FROM space WHERE network = ?1 AND account = ?2 AND id = ?3", key)?;
        }
        for space in spaces {
            tx.execute(
                "INSERT INTO space (network, account, id, name, icon, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (network, account, id) DO UPDATE SET
                     name = excluded.name, icon = excluded.icon, position = excluded.position",
                params![network, id, space.id, space.name, text(&space.icon), space.position],
            )?;
        }
        tx.commit()
    }

    pub fn spaces(&self, network: Option<Network>) -> rusqlite::Result<Vec<Space>> {
        let mut query = self.db.prepare(&format!(
            "SELECT s.network, s.account, s.id, s.name, s.position,
                    (SELECT COALESCE(SUM({ATTENTION}), 0) FROM conversation c
                      WHERE c.network = s.network AND c.account = s.account AND c.space = s.id),
                    EXISTS (SELECT 1 FROM conversation c
                      WHERE c.network = s.network AND c.account = s.account AND c.space = s.id
                        AND c.unread > 0 AND NOT c.muted),
                    m.id, m.kind, m.width, m.height, m.bytes, m.path, m.failed,
                    (SELECT f.id FROM folder f
                      WHERE f.network = s.network AND f.account = s.account
                        AND instr(f.spaces, ',' || s.id || ',') > 0 LIMIT 1),
                    s.muted, s.hide_muted
             FROM space s
             LEFT JOIN media m ON m.network = s.network AND m.account = s.account AND m.id = s.icon
             WHERE ?1 IS NULL OR s.network = ?1
             ORDER BY s.network, s.account, s.position, s.id"
        ))?;
        let rows = query.query_map(params![network.map(|network| word(&network))], |row| {
            Ok(Space {
                key: key_at(row)?,
                name: row.get(3)?,
                position: row.get(4)?,
                attention: row.get(5)?,
                unread: row.get(6)?,
                icon: file_at(row, 7)?,
                folder: row.get(14)?,
                muted: row.get(15)?,
                hide_muted: row.get(16)?,
            })
        })?;
        rows.collect()
    }

    /// One Space's groups and conversations, all of them. What is no longer in it goes; unread
    /// state, previews, stars and collapsed groups of what stays are kept.
    pub fn replace_tree(
        &mut self,
        space: &Key,
        groups: &[Group],
        conversations: &[ConversationRow],
    ) -> rusqlite::Result<()> {
        let account = space.account();
        let tx = self.db.transaction()?;
        let key = params![word(&space.network), space.account, space.id];
        // A thread that names its parent is not the tree's to list: it stays while its parent does.
        tx.execute(
            "UPDATE conversation SET stale = 1
              WHERE network = ?1 AND account = ?2 AND space = ?3 AND parent = ''",
            key,
        )?;
        for row in conversations {
            let mut row = row.clone();
            row.space = Some(space.id.clone());
            upsert_conversation(&tx, &account, &row)?;
        }
        tx.execute(
            "DELETE FROM message WHERE network = ?1 AND account = ?2 AND conversation IN
                 (SELECT id FROM conversation
                   WHERE network = ?1 AND account = ?2 AND space = ?3 AND stale = 1)",
            key,
        )?;
        tx.execute(
            "DELETE FROM conversation
              WHERE network = ?1 AND account = ?2 AND space = ?3 AND stale = 1",
            key,
        )?;
        drop_orphan_threads(&tx, &account)?;
        let collapsed: Vec<String> = tx
            .prepare(
                "SELECT id FROM section
                  WHERE network = ?1 AND account = ?2 AND space = ?3 AND collapsed",
            )?
            .query_map(key, |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        tx.execute("DELETE FROM section WHERE network = ?1 AND account = ?2 AND space = ?3", key)?;
        for group in groups {
            tx.execute(
                "INSERT OR REPLACE INTO section
                     (network, account, space, id, name, position, collapsed, kind, muted, unread_only)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    word(&space.network),
                    space.account,
                    space.id,
                    group.id,
                    group.name,
                    group.position,
                    collapsed.contains(&group.id),
                    word(&group.kind),
                    group.muted,
                    group.unread_only,
                ],
            )?;
        }
        tx.commit()
    }

    /// One group created, renamed or moved, outside a tree replacement. Whether the user
    /// collapsed it is kept.
    pub fn upsert_group(&mut self, space: &Key, group: &Group) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO section (network, account, space, id, name, position, kind, muted, unread_only)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (network, account, space, id) DO UPDATE SET
                 name = excluded.name, position = excluded.position, kind = excluded.kind,
                 muted = excluded.muted, unread_only = excluded.unread_only",
            params![
                word(&space.network),
                space.account,
                space.id,
                group.id,
                group.name,
                group.position,
                word(&group.kind),
                group.muted,
                group.unread_only
            ],
        )?;
        Ok(())
    }

    /// True if it was there. Its conversations stay, under no group.
    pub fn remove_group(&mut self, space: &Key, group: &str) -> rusqlite::Result<bool> {
        let tx = self.db.transaction()?;
        let key = params![word(&space.network), space.account, space.id, group];
        let removed = tx.execute(
            "DELETE FROM section WHERE network = ?1 AND account = ?2 AND space = ?3 AND id = ?4",
            key,
        )?;
        tx.execute(
            "UPDATE conversation SET section = ''
              WHERE network = ?1 AND account = ?2 AND space = ?3 AND section = ?4",
            key,
        )?;
        tx.commit()?;
        Ok(removed == 1)
    }

    /// A Space's groups, and one page of its conversations in the network's order. `None` when
    /// there is no such Space.
    pub fn tree(
        &self,
        space: &Key,
        offset: u32,
        limit: u32,
    ) -> rusqlite::Result<Option<Tree>> {
        let key = params![word(&space.network), space.account, space.id];
        let known: bool = self.db.query_row(
            "SELECT EXISTS (SELECT 1 FROM space WHERE network = ?1 AND account = ?2 AND id = ?3)",
            key,
            |row| row.get(0),
        )?;
        if !known {
            return Ok(None);
        }
        let groups = self
            .db
            .prepare(
                "SELECT g.id, g.name, g.position, g.collapsed, g.kind,
                        (SELECT COUNT(*) FROM conversation c
                          WHERE c.network = g.network AND c.account = g.account
                            AND c.space = g.space AND c.section = g.id),
                        g.muted, g.unread_only
                   FROM section g
                  WHERE g.network = ?1 AND g.account = ?2 AND g.space = ?3 ORDER BY g.position, g.id",
            )?
            .query_map(key, |row| {
                Ok(Group {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    position: row.get(2)?,
                    collapsed: row.get(3)?,
                    kind: from_word(row.get(4)?)?,
                    count: row.get(5)?,
                    muted: row.get(6)?,
                    unread_only: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let total = self.db.query_row(
            "SELECT COUNT(*) FROM conversation WHERE network = ?1 AND account = ?2 AND space = ?3",
            key,
            |row| row.get(0),
        )?;
        let conversations = self
            .db
            .prepare(&format!(
                "{CONVERSATION} WHERE c.network = ?1 AND c.account = ?2 AND c.space = ?3
                 ORDER BY c.position, c.id LIMIT ?4 OFFSET ?5"
            ))?
            .query_map(
                params![word(&space.network), space.account, space.id, limit, offset],
                conversation_from,
            )?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(Tree {
            groups,
            conversations,
            total,
        }))
    }

    pub fn set_collapsed(&mut self, space: &Key, group: &str, collapsed: bool) -> rusqlite::Result<bool> {
        let changed = self.db.execute(
            "UPDATE section SET collapsed = ?5
              WHERE network = ?1 AND account = ?2 AND space = ?3 AND id = ?4",
            params![word(&space.network), space.account, space.id, group, collapsed],
        )?;
        Ok(changed == 1)
    }

    // Conversations.

    /// Records one conversation outside a tree replacement: a direct message, a channel the
    /// network just created.
    pub fn upsert_conversation(&mut self, account: &AccountKey, row: &ConversationRow) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        upsert_conversation(&tx, account, row)?;
        tx.commit()
    }

    pub fn remove_conversation(&mut self, conversation: &Key) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        let key = params![word(&conversation.network), conversation.account, conversation.id];
        tx.execute("DELETE FROM message WHERE network = ?1 AND account = ?2 AND conversation = ?3", key)?;
        tx.execute("DELETE FROM conversation WHERE network = ?1 AND account = ?2 AND id = ?3", key)?;
        drop_orphan_threads(&tx, &conversation.account())?;
        tx.commit()
    }

    pub fn conversation(&self, conversation: &Key) -> rusqlite::Result<Option<Conversation>> {
        self.db
            .query_row(
                &format!("{CONVERSATION} WHERE c.network = ?1 AND c.account = ?2 AND c.id = ?3"),
                params![word(&conversation.network), conversation.account, conversation.id],
                conversation_from,
            )
            .optional()
    }

    /// What the Inbox holds (`IN_INBOX`, said in words on `Request::Inbox`). All: starred
    /// first, then newest first. Unread and Mentions: mentions first, then newest. Also how
    /// many rows match in all.
    pub fn inbox(
        &self,
        network: Option<Network>,
        filter: InboxFilter,
        offset: u32,
        limit: u32,
    ) -> rusqlite::Result<(Vec<Conversation>, u32)> {
        let narrowed = match filter {
            InboxFilter::All => "1",
            InboxFilter::Unread => IS_UNREAD,
            InboxFilter::Mentions => IS_MENTION,
            InboxFilter::Directs => "c.kind IN ('direct', 'group')",
        };
        let belongs = format!("{IN_INBOX} AND (?1 IS NULL OR c.network = ?1) AND {narrowed}");
        let first = match filter {
            InboxFilter::Unread | InboxFilter::Mentions => "c.mentions > 0",
            InboxFilter::All | InboxFilter::Directs => "c.starred",
        };
        let network = network.map(|network| word(&network));
        let total = self.db.query_row(
            &format!("SELECT COUNT(*) FROM conversation c WHERE {belongs}"),
            params![network],
            |row| row.get(0),
        )?;
        let rows = self
            .db
            .prepare(&format!(
                "{CONVERSATION} WHERE {belongs}
                 ORDER BY {first} DESC, c.updated DESC, c.network, c.account, c.id
                 LIMIT ?2 OFFSET ?3"
            ))?
            .query_map(params![network, limit, offset], conversation_from)?
            .collect::<rusqlite::Result<_>>()?;
        Ok((rows, total))
    }

    /// How many rows each Inbox filter holds, in one pass.
    pub fn inbox_counts(&self, network: Option<Network>) -> rusqlite::Result<InboxCounts> {
        self.db.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM({IS_UNREAD}), 0), COALESCE(SUM({IS_MENTION}), 0),
                        COALESCE(SUM({IS_APP}), 0)
                   FROM conversation c WHERE {IN_INBOX} AND (?1 IS NULL OR c.network = ?1)"
            ),
            params![network.map(|network| word(&network))],
            counts_from,
        )
    }

    /// A network's own list of the conversations that belong to no Space: its pins in order,
    /// then newest first (`Request::Home`). With a folder: what is in it, its own pins first.
    /// Answers a page, how many rows match, and their counts.
    pub fn home(
        &self,
        network: Network,
        account: Option<&AccountKey>,
        folder: Option<&str>,
        shelf: Shelf,
        offset: u32,
        limit: u32,
    ) -> rusqlite::Result<(Vec<Conversation>, u32, InboxCounts)> {
        let shelf = match shelf {
            Shelf::Main => "NOT c.archived AND c.special != 'request'",
            Shelf::Archive => "c.archived AND c.special != 'request'",
            Shelf::Requests => "c.special = 'request'",
        };
        let belongs = format!(
            "c.network = ?1 AND (?2 IS NULL OR c.account = ?2) AND c.space = '' AND c.parent = ''
             AND NOT c.hidden
             AND CASE WHEN ?3 IS NULL THEN {shelf}
                      ELSE instr(c.folders, ',' || ?3 || ',') > 0 AND c.special != 'request' END"
        );
        let in_folder_pins = "(SELECT instr(f.pinned, ',' || c.id || ',') FROM folder f
                                WHERE f.network = c.network AND f.account = c.account AND f.id = ?3)";
        let key = params![word(&network), account.map(|account| account.account.as_str()), folder];
        let counts = self.db.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM({IS_UNREAD}), 0), COALESCE(SUM({IS_MENTION}), 0),
                        COALESCE(SUM({IS_APP}), 0)
                   FROM conversation c WHERE {belongs}"
            ),
            key,
            counts_from,
        )?;
        let rows = self
            .db
            .prepare(&format!(
                "{CONVERSATION} WHERE {belongs}
                 ORDER BY CASE WHEN ?3 IS NULL THEN coalesce(c.pin, 2147483647)
                               ELSE coalesce(nullif({in_folder_pins}, 0), 2147483647) END,
                          c.updated DESC, c.account, c.id
                 LIMIT ?4 OFFSET ?5"
            ))?
            .query_map(
                params![word(&network), account.map(|account| account.account.as_str()), folder, limit, offset],
                conversation_from,
            )?
            .collect::<rusqlite::Result<_>>()?;
        Ok((rows, counts.all, counts))
    }

    /// A forum's topics: its pins in order, then newest first, and how many there are.
    pub fn topics(&self, forum: &Key, offset: u32, limit: u32) -> rusqlite::Result<(Vec<Conversation>, u32)> {
        let belongs = "c.network = ?1 AND c.account = ?2 AND c.parent = ?3 AND NOT c.hidden";
        let key = params![word(&forum.network), forum.account, forum.id];
        let total = self.db.query_row(&format!("SELECT COUNT(*) FROM conversation c WHERE {belongs}"), key, |row| row.get(0))?;
        let rows = self
            .db
            .prepare(&format!(
                "{CONVERSATION} WHERE {belongs}
                 ORDER BY coalesce(c.pin, 2147483647), c.updated DESC, c.id LIMIT ?4 OFFSET ?5"
            ))?
            .query_map(params![word(&forum.network), forum.account, forum.id, limit, offset], conversation_from)?
            .collect::<rusqlite::Result<_>>()?;
        Ok((rows, total))
    }

    /// The followed threads of a Space, newest first, and how many there are.
    pub fn threads(&self, space: &Key, offset: u32, limit: u32) -> rusqlite::Result<(Vec<Conversation>, u32)> {
        let belongs = "c.network = ?1 AND c.account = ?2 AND c.kind = 'thread' AND c.followed
             AND (c.space = ?3 OR c.parent IN (SELECT p.id FROM conversation p
                   WHERE p.network = ?1 AND p.account = ?2 AND p.space = ?3))";
        let key = params![word(&space.network), space.account, space.id];
        let total = self.db.query_row(&format!("SELECT COUNT(*) FROM conversation c WHERE {belongs}"), key, |row| row.get(0))?;
        let rows = self
            .db
            .prepare(&format!("{CONVERSATION} WHERE {belongs} ORDER BY c.updated DESC, c.id LIMIT ?4 OFFSET ?5"))?
            .query_map(params![word(&space.network), space.account, space.id, limit, offset], conversation_from)?
            .collect::<rusqlite::Result<_>>()?;
        Ok((rows, total))
    }

    /// Where the network files a conversation. True if anything changed.
    pub fn set_filing(&mut self, conversation: &Key, filing: &Filing) -> rusqlite::Result<bool> {
        let changed = self.db.execute(
            "UPDATE conversation SET folders = ?4, archived = ?5, pin = ?6, hidden = ?7, followed = ?8
              WHERE network = ?1 AND account = ?2 AND id = ?3
                AND (folders != ?4 OR archived != ?5 OR pin IS NOT ?6 OR hidden != ?7 OR followed != ?8)",
            params![
                word(&conversation.network),
                conversation.account,
                conversation.id,
                id_list(&filing.folders, FOLDERS_PER_ROW),
                filing.archived,
                filing.pin,
                filing.hidden,
                filing.followed,
            ],
        )?;
        Ok(changed == 1)
    }

    // Folders.

    /// The account's folders, all of them, in the network's order. A folder no longer listed goes.
    pub fn replace_folders(&mut self, account: &AccountKey, folders: &[FolderRow]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        let key = params![word(&account.network), account.account];
        tx.execute("DELETE FROM folder WHERE network = ?1 AND account = ?2", key)?;
        for folder in folders.iter().take(FOLDERS_LIMIT) {
            let mut name = folder.name.clone();
            clip(&mut name, 100);
            tx.execute(
                "INSERT OR REPLACE INTO folder (network, account, id, name, position, color, spaces, pinned)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    word(&account.network),
                    account.account,
                    folder.id.replace(',', ""),
                    name,
                    folder.position,
                    folder.color,
                    id_list(&folder.spaces, FOLDER_MEMBERS),
                    id_list(&folder.pinned, FOLDER_MEMBERS),
                ],
            )?;
        }
        tx.commit()
    }

    /// A network's folders, or every network's, with what is unread in each.
    pub fn folders(&self, network: Option<Network>) -> rusqlite::Result<Vec<Folder>> {
        let inside = "c.network = f.network AND c.account = f.account AND NOT c.hidden
                      AND (instr(c.folders, ',' || f.id || ',') > 0
                           OR (c.space != '' AND instr(f.spaces, ',' || c.space || ',') > 0))";
        self.db
            .prepare(&format!(
                "SELECT f.network, f.account, f.id, f.name, f.position, f.color, f.spaces,
                        (SELECT COUNT(*) FROM conversation c WHERE {inside} AND NOT c.muted
                           AND (c.unread > 0 OR c.mentions > 0)),
                        (SELECT COALESCE(SUM(c.mentions), 0) FROM conversation c WHERE {inside})
                   FROM folder f WHERE ?1 IS NULL OR f.network = ?1
                  ORDER BY f.network, f.account, f.position, f.id"
            ))?
            .query_map(params![network.map(|network| word(&network))], |row| {
                Ok(Folder {
                    key: key_at(row)?,
                    name: row.get(3)?,
                    position: row.get(4)?,
                    color: row.get(5)?,
                    spaces: from_id_list(&row.get::<_, String>(6)?),
                    unread: row.get(7)?,
                    mentions: row.get(8)?,
                })
            })?
            .collect()
    }

    /// A Space's own settings: muted as a whole, and the user's "hide muted channels".
    pub fn set_space_flags(&mut self, space: &Key, muted: bool, hide_muted: bool) -> rusqlite::Result<bool> {
        let changed = self.db.execute(
            "UPDATE space SET muted = ?4, hide_muted = ?5
              WHERE network = ?1 AND account = ?2 AND id = ?3 AND (muted != ?4 OR hide_muted != ?5)",
            params![word(&space.network), space.account, space.id, muted, hide_muted],
        )?;
        Ok(changed == 1)
    }

    // Previews.

    /// The newest message as a line, for a network that lists only that. Kept only when it is
    /// not older than what the row already shows.
    pub fn set_preview(&mut self, conversation: &Key, line: &PreviewLine) -> rusqlite::Result<bool> {
        let mut text = line.text.clone();
        clip(&mut text, 120);
        let mut sender = if line.mine { String::new() } else { line.sender.clone() };
        clip(&mut sender, 80);
        let changed = self.db.execute(
            "UPDATE conversation SET preview = ?4, preview_from = ?5, preview_mine = ?6, preview_id = ?7,
                    updated = max(updated, ?8)
              WHERE network = ?1 AND account = ?2 AND id = ?3 AND ?8 >= updated
                AND (preview != ?4 OR preview_from != ?5 OR preview_mine != ?6 OR preview_id != ?7 OR updated != ?8)",
            params![
                word(&conversation.network),
                conversation.account,
                conversation.id,
                text,
                sender,
                line.mine,
                line.id,
                line.sent
            ],
        )?;
        Ok(changed == 1)
    }

    /// The other side has read my messages up to `message_id`. True if the row changed.
    pub fn set_seen(&mut self, conversation: &Key, message_id: &str) -> rusqlite::Result<bool> {
        let changed = self.db.execute(
            "UPDATE conversation SET seen_to = ?4
              WHERE network = ?1 AND account = ?2 AND id = ?3
                AND (seen_to = '' OR (length(seen_to), seen_to) < (length(?4), ?4))",
            params![word(&conversation.network), conversation.account, conversation.id, message_id],
        )?;
        Ok(changed == 1)
    }

    /// Of these rows, the ones with no preview line yet.
    pub fn previewless(&self, rows: &[Key]) -> rusqlite::Result<Vec<Key>> {
        let mut query = self.db.prepare(
            "SELECT 1 FROM conversation WHERE network = ?1 AND account = ?2 AND id = ?3 AND preview = ''",
        )?;
        let mut missing = Vec::new();
        for row in rows {
            if query.exists(params![word(&row.network), row.account, row.id])? {
                missing.push(row.clone());
            }
        }
        Ok(missing)
    }

    /// Conversations whose title contains `query`, most recently active first.
    pub fn find(&self, network: Option<Network>, query: &str, limit: u32) -> rusqlite::Result<Vec<Conversation>> {
        let pattern = format!("%{}", like_prefix(query));
        self.db
            .prepare(&format!(
                "{CONVERSATION} WHERE (?1 IS NULL OR c.network = ?1) AND c.title LIKE ?2 ESCAPE '\\'
                 ORDER BY c.updated DESC, c.network, c.account, c.id LIMIT ?3"
            ))?
            .query_map(params![network.map(|network| word(&network)), pattern, limit], conversation_from)?
            .collect()
    }

    /// What the user typed and did not send. False when there is no such conversation.
    pub fn set_draft(&mut self, conversation: &Key, draft: &str) -> rusqlite::Result<bool> {
        let mut draft = draft.to_owned();
        clip(&mut draft, DRAFT_LIMIT);
        let changed = self.db.execute(
            "UPDATE conversation SET draft = ?4 WHERE network = ?1 AND account = ?2 AND id = ?3",
            params![word(&conversation.network), conversation.account, conversation.id, draft],
        )?;
        Ok(changed == 1)
    }

    /// The unread state the network reported: at connect, or when another device read it. The
    /// preview is left alone; it comes from messages.
    /// The network's counts. Mentions going up are noticed: the Inbox keeps a channel a day
    /// after it mentioned me.
    pub fn set_counts(
        &mut self,
        conversation: &Key,
        unread: u32,
        mentions: u32,
        last_read: Option<&str>,
        updated: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.db.execute(
            "UPDATE conversation SET
                 noticed = CASE WHEN ?5 > mentions THEN CAST(strftime('%s', 'now') AS INTEGER) ELSE noticed END,
                 unread = ?4, mentions = ?5, updated = max(updated, ?7),
                 last_read = CASE WHEN ?6 IS NULL THEN last_read ELSE ?6 END
              WHERE network = ?1 AND account = ?2 AND id = ?3
                AND (unread != ?4 OR mentions != ?5 OR updated < ?7
                     OR (?6 IS NOT NULL AND last_read != ?6))",
            params![
                word(&conversation.network),
                conversation.account,
                conversation.id,
                unread,
                mentions,
                last_read,
                updated
            ],
        )?;
        Ok(changed == 1)
    }

    fn set_flag(&mut self, conversation: &Key, column: &str, on: bool) -> rusqlite::Result<bool> {
        let changed = self.db.execute(
            &format!(
                "UPDATE conversation SET {column} = ?4
                  WHERE network = ?1 AND account = ?2 AND id = ?3 AND {column} != ?4"
            ),
            params![word(&conversation.network), conversation.account, conversation.id, on],
        )?;
        Ok(changed == 1)
    }

    pub fn set_muted(&mut self, conversation: &Key, muted: bool) -> rusqlite::Result<bool> {
        self.set_flag(conversation, "muted", muted)
    }

    pub fn set_starred(&mut self, conversation: &Key, starred: bool) -> rusqlite::Result<bool> {
        self.set_flag(conversation, "starred", starred)
    }

    /// The user read the conversation up to `message_id`. True if anything was unread.
    pub fn mark_read(&mut self, conversation: &Key, message_id: &str) -> rusqlite::Result<bool> {
        let changed = self.db.execute(
            "UPDATE conversation SET unread = 0, mentions = 0, last_read = ?4
              WHERE network = ?1 AND account = ?2 AND id = ?3
                AND (unread != 0 OR mentions != 0 OR last_read != ?4)",
            params![word(&conversation.network), conversation.account, conversation.id, message_id],
        )?;
        Ok(changed == 1)
    }

    // Messages.

    /// Records a message: new, or the newer form of one already stored (an edit, a reaction, a
    /// link preview arriving). The conversation must exist.
    ///
    /// `unread` says whether a new incoming message counts: live ones do, history does not,
    /// because the network already said how much of it is unread. A new outgoing message that is
    /// the newest clears the count: replying from any device means it was read. A message that
    /// carries the `client_id` of one still pending here replaces it.
    pub fn ingest(&mut self, conversation: &Key, message: &Message, unread: bool) -> rusqlite::Result<Stored> {
        let tx = self.db.transaction()?;
        let (network, account, id) = (word(&conversation.network), &conversation.account, &conversation.id);
        if let Some(client_id) = message.client_id.as_deref().filter(|client_id| *client_id != message.id) {
            tx.execute(
                "DELETE FROM message WHERE network = ?1 AND account = ?2 AND conversation = ?3
                    AND id = ?4 AND pending != 0",
                params![network, account, id, client_id],
            )?;
        }
        let mut stored = message.clone();
        stored.bound();
        stored.id.clear();
        stored.text.clear();
        let body = serde_json::to_string(&stored).map_err(broken)?;
        let pending = match message.state {
            SendState::Sent => 0,
            SendState::Pending => 1,
            SendState::Failed { .. } => 2,
        };
        let before: Option<(String, String)> = tx
            .query_row(
                "SELECT text, body FROM message
                  WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND id = ?4",
                params![network, account, id, message.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let outcome = match before {
            Some((text, old)) if text == message.text && old == body => Stored::Same,
            Some(_) => Stored::Updated,
            None => Stored::New,
        };
        if outcome != Stored::Same {
            tx.execute(
                "INSERT OR REPLACE INTO message
                     (network, account, conversation, id, sent, text, pending, client_id, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![network, account, id, message.id, message.sent, message.text, pending, message.client_id, body],
            )?;
        }
        let counts = unread && !message.outgoing;
        match outcome {
            Stored::New => {
                tx.execute(
                    "UPDATE conversation SET
                         unread = CASE WHEN ?7 AND ?5 >= updated THEN 0 ELSE unread + ?8 END,
                         mentions = CASE WHEN ?7 AND ?5 >= updated THEN 0 ELSE mentions + ?9 END,
                         last_read = CASE WHEN ?7 AND ?5 >= updated AND ?10 = 0 THEN ?6 ELSE last_read END,
                         noticed = CASE WHEN ?9 > 0 THEN max(noticed, ?5) ELSE noticed END,
                         preview_from = CASE WHEN ?5 >= updated THEN ?11 ELSE preview_from END,
                         preview_mine = CASE WHEN ?5 >= updated THEN ?7 ELSE preview_mine END,
                         preview_id = CASE WHEN ?5 >= updated THEN ?6 ELSE preview_id END,
                         preview = CASE WHEN ?5 >= updated THEN ?4 ELSE preview END,
                         updated = max(updated, ?5)
                      WHERE network = ?1 AND account = ?2 AND id = ?3",
                    params![
                        network,
                        account,
                        id,
                        preview_of(message),
                        message.sent,
                        message.id,
                        message.outgoing,
                        u32::from(counts),
                        u32::from(counts && message.mentions_me),
                        pending,
                        if message.outgoing { "" } else { message.sender.name.as_str() },
                    ],
                )?;
            }
            // An edit of the newest message is what the row previews.
            Stored::Updated => {
                tx.execute(
                    "UPDATE conversation SET preview = ?4, preview_id = ?6, preview_mine = ?7, preview_from = ?8
                      WHERE network = ?1 AND account = ?2 AND id = ?3 AND updated = ?5",
                    params![
                        network,
                        account,
                        id,
                        preview_of(message),
                        message.sent,
                        message.id,
                        message.outgoing,
                        if message.outgoing { "" } else { message.sender.name.as_str() },
                    ],
                )?;
            }
            Stored::Same => {}
        }
        tx.commit()?;
        Ok(outcome)
    }

    pub fn message(&self, conversation: &Key, id: &str) -> rusqlite::Result<Option<Message>> {
        self.db
            .query_row(
                "SELECT id, text, body FROM message
                  WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND id = ?4",
                params![word(&conversation.network), conversation.account, conversation.id, id],
                message_from,
            )
            .optional()
    }

    /// True if it was there. A message with a thread hanging off it stays as a tombstone, and
    /// every stored reply to the message says its original is gone. The preview is left to the
    /// next message.
    pub fn delete_message(&mut self, conversation: &Key, id: &str) -> rusqlite::Result<bool> {
        let Some(mut message) = self.message(conversation, id)? else {
            return Ok(false);
        };
        if message.thread.is_some() && message.state == SendState::Sent {
            if message.deleted {
                return Ok(false);
            }
            message.tombstone();
            self.ingest(conversation, &message, false)?;
        } else {
            self.db.execute(
                "DELETE FROM message WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND id = ?4",
                params![word(&conversation.network), conversation.account, conversation.id, id],
            )?;
        }
        // The body is the protocol's JSON, so a reply names its original in exactly this form.
        let naming = format!("\"reply_to\":{{\"id\":{}", serde_json::to_string(id).map_err(broken)?);
        let replies: Vec<String> = self
            .db
            .prepare(
                "SELECT id FROM message
                  WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND instr(body, ?4) > 0",
            )?
            .query_map(params![word(&conversation.network), conversation.account, conversation.id, naming], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for reply in replies {
            let Some(mut reply) = self.message(conversation, &reply)? else {
                continue;
            };
            if let Some(quoted) = reply.reply_to.as_mut().filter(|quoted| quoted.id == id) {
                quoted.deleted = true;
                quoted.snippet.clear();
                self.ingest(conversation, &reply, false)?;
            }
        }
        Ok(true)
    }

    /// One person adding or removing one reaction. `mine` is set when that person is us, and
    /// ours counts once however often it is reported: the shell's own toggle and the network's
    /// word of it are the same reaction. `emoji` may also be the id of a custom emoji's file.
    pub fn react(
        &mut self,
        conversation: &Key,
        message_id: &str,
        reaction: &Reaction,
        added: bool,
    ) -> rusqlite::Result<bool> {
        let Some(mut message) = self.message(conversation, message_id)? else {
            return Ok(false);
        };
        let same = |have: &Reaction| {
            have.emoji == reaction.emoji || have.file.as_ref().is_some_and(|file| file.id == reaction.emoji)
        };
        let at = message.reactions.iter().position(same);
        match (at, added) {
            (Some(at), true) if reaction.mine && message.reactions[at].mine => return Ok(false),
            (Some(at), false) if reaction.mine && !message.reactions[at].mine => return Ok(false),
            (Some(at), true) => {
                message.reactions[at].count += 1;
                message.reactions[at].mine |= reaction.mine;
            }
            (None, true) => message.reactions.push(Reaction {
                count: 1,
                ..reaction.clone()
            }),
            (Some(at), false) => {
                let have = &mut message.reactions[at];
                have.count = have.count.saturating_sub(1);
                have.mine &= !reaction.mine;
                if have.count == 0 {
                    message.reactions.remove(at);
                }
            }
            (None, false) => return Ok(false),
        }
        Ok(self.ingest(conversation, &message, false)? != Stored::Same)
    }

    /// The ids stored between two message ids, both included, oldest first: what an adapter
    /// compares a page from the network against to find what was deleted while it was away.
    /// Ids are compared as the numbers they are written as (shorter first, then by text).
    pub fn ids_between(&self, conversation: &Key, oldest: &str, newest: &str, limit: u32) -> rusqlite::Result<Vec<String>> {
        self.db
            .prepare(&format!(
                "SELECT id FROM message
                  WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND pending = 0
                    AND (length(id), id) >= (length(?4), ?4) AND (length(id), id) <= (length(?5), ?5)
                  {OLDEST_FIRST} LIMIT ?6"
            ))?
            .query_map(
                params![word(&conversation.network), conversation.account, conversation.id, oldest, newest, limit],
                |row| row.get(0),
            )?
            .collect()
    }

    /// What was shared in a conversation, newest first: its pictures and videos, its other
    /// files, or its links. From the cache alone. Looks at no more than [`KEEP_MESSAGES`]
    /// messages below `before`, and says whether older ones were left unlooked at. `None` when
    /// there is no such conversation.
    pub fn shared(
        &self,
        conversation: &Key,
        kind: SharedKind,
        before: Option<&str>,
        limit: u32,
    ) -> rusqlite::Result<Option<(Vec<SharedItem>, bool)>> {
        if self.conversation(conversation)?.is_none() {
            return Ok(None);
        }
        // The body is the protocol's JSON, so a message that holds one says so in these words.
        let holds = match kind {
            SharedKind::Media | SharedKind::Files => "instr(body, '\"attachments\"') > 0",
            SharedKind::Links => "(instr(body, '\"previews\"') > 0 OR instr(body, '\"style\":\"link\"') > 0)",
        };
        let below = match before {
            Some(_) => "AND (sent, length(id), id) < (SELECT sent, length(id), id FROM message
                           WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND id = ?4)",
            None => "AND ?4 IS NULL",
        };
        let mut query = self.db.prepare(&format!(
            "SELECT id, text, body FROM message
              WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND pending = 0 {below} AND {holds}
              {NEWEST_FIRST} LIMIT ?5"
        ))?;
        let key = params![word(&conversation.network), conversation.account, conversation.id, before, KEEP_MESSAGES];
        let mut items: Vec<SharedItem> = Vec::new();
        let mut more = false;
        for message in query.query_map(key, message_from)? {
            let message = message?;
            if items.len() >= limit as usize {
                more = true;
                break;
            }
            let item = |attachment, url: &str, title: &str| SharedItem {
                message_id: message.id.clone(),
                sent: message.sent,
                sender: message.sender.name.clone(),
                attachment,
                url: url.to_owned(),
                title: title.to_owned(),
            };
            let pictured = |kind: protocol::MediaKind| {
                matches!(kind, protocol::MediaKind::Image | protocol::MediaKind::Video | protocol::MediaKind::Gif)
            };
            match kind {
                SharedKind::Media | SharedKind::Files => {
                    let wanted = message.attachments.iter().filter(|attachment| {
                        !attachment.sticker && pictured(attachment.file.kind) == (kind == SharedKind::Media)
                    });
                    items.extend(wanted.map(|attachment| item(Some(attachment.clone()), "", "")));
                }
                SharedKind::Links => {
                    let mut seen: Vec<&str> = Vec::new();
                    for preview in &message.previews {
                        seen.push(&preview.url);
                        items.push(item(None, &preview.url, &preview.title));
                    }
                    for span in &message.spans {
                        if let protocol::Style::Link { url } = &span.style {
                            if !seen.contains(&url.as_str()) {
                                seen.push(url);
                                items.push(item(None, url, ""));
                            }
                        }
                    }
                }
            }
        }
        let mut path = self.db.prepare(
            "SELECT path, bytes, failed FROM media WHERE network = ?1 AND account = ?2 AND id = ?3",
        )?;
        let files = items.iter_mut().filter_map(|item| item.attachment.as_mut());
        for file in files.flat_map(|attachment| std::iter::once(&mut attachment.file).chain(&mut attachment.thumb)) {
            let known: Option<(Option<String>, i64, Option<String>)> = path
                .query_row(params![word(&conversation.network), conversation.account, file.id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .optional()?;
            match known {
                Some((Some(on_disk), bytes, _)) => {
                    file.path = Some(on_disk);
                    file.bytes = bytes as u64;
                }
                Some((None, _, failed)) => file.failed = failed,
                None => {}
            }
        }
        Ok(Some((items, more)))
    }

    /// The id of the newest message the network holds for a conversation.
    pub fn newest(&self, conversation: &Key) -> rusqlite::Result<Option<String>> {
        self.db
            .query_row(
                &format!(
                    "SELECT id FROM message
                      WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND pending = 0
                      {NEWEST_FIRST} LIMIT 1"
                ),
                params![word(&conversation.network), conversation.account, conversation.id],
                |row| row.get(0),
            )
            .optional()
    }

    /// One page of a conversation, oldest first, with file paths filled in. Notes that the
    /// conversation was opened, which retention reads. `None` when it does not exist.
    pub fn messages(
        &mut self,
        conversation: &Key,
        page: &Page,
        limit: u32,
        now: i64,
    ) -> rusqlite::Result<Option<MessagePage>> {
        let key = [
            Value::Text(word(&conversation.network)),
            Value::Text(conversation.account.clone()),
            Value::Text(conversation.id.clone()),
        ];
        let last_read: Option<String> = self
            .db
            .query_row(
                "UPDATE conversation SET opened = ?4
                  WHERE network = ?1 AND account = ?2 AND id = ?3 RETURNING last_read",
                params![key[0], key[1], key[2], now],
                |row| row.get(0),
            )
            .optional()?;
        let Some(last_read) = last_read else {
            return Ok(None);
        };
        // One more than asked for says whether there is more in that direction.
        let run = |side: &str, order: &str, anchor: Option<&str>, limit: u32| -> rusqlite::Result<(Vec<Message>, bool)> {
            let mut values = key.to_vec();
            let place = match anchor {
                Some(anchor) => {
                    values.push(Value::Text(anchor.to_owned()));
                    format!(
                        "AND (sent, length(id), id) {side} (SELECT sent, length(id), id FROM message
                           WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND id = ?4)"
                    )
                }
                None => String::new(),
            };
            let mut found: Vec<Message> = self
                .db
                .prepare(&format!(
                    "SELECT id, text, body FROM message
                      WHERE network = ?1 AND account = ?2 AND conversation = ?3 {place}
                      {order} LIMIT {}",
                    limit + 1
                ))?
                .query_map(params_from_iter(values), message_from)?
                .collect::<rusqlite::Result<_>>()?;
            let more = found.len() > limit as usize;
            found.truncate(limit as usize);
            Ok((found, more))
        };
        let (mut messages, older, newer) = match page {
            Page::Newest => {
                let (mut found, older) = run("<", NEWEST_FIRST, None, limit)?;
                found.reverse();
                (found, older, false)
            }
            Page::Before { id } => {
                let (mut found, older) = run("<", NEWEST_FIRST, Some(id), limit)?;
                found.reverse();
                (found, older, true)
            }
            Page::After { id } => {
                let (found, newer) = run(">", OLDEST_FIRST, Some(id), limit)?;
                (found, true, newer)
            }
            Page::Around { id } => {
                let (mut found, older) = run("<=", NEWEST_FIRST, Some(id), limit / 2 + 1)?;
                found.reverse();
                let (after, newer) = run(">", OLDEST_FIRST, Some(id), limit / 2)?;
                found.extend(after);
                (found, older, newer)
            }
        };
        let mut path = self.db.prepare(
            "SELECT path, bytes, failed FROM media WHERE network = ?1 AND account = ?2 AND id = ?3",
        )?;
        for file in messages.iter_mut().flat_map(Message::files_mut) {
            let known: Option<(Option<String>, i64, Option<String>)> = path
                .query_row(params![key[0], key[1], file.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .optional()?;
            match known {
                Some((Some(on_disk), bytes, _)) => {
                    file.path = Some(on_disk);
                    file.bytes = bytes as u64;
                }
                Some((None, _, failed)) => file.failed = failed,
                None => {}
            }
        }
        drop(path);
        Ok(Some(MessagePage {
            messages,
            last_read: some(last_read),
            older,
            newer,
        }))
    }

    /// Pending messages older than `before` did not go: the adapter never answered, or chatd was
    /// restarted. They become `Failed` and wait for the user; nothing is ever sent late on its
    /// own. Returns the conversations touched.
    pub fn fail_pending(&mut self, before: i64, reason: &str) -> rusqlite::Result<Vec<Key>> {
        let stuck: Vec<(Key, String)> = self
            .db
            .prepare(
                "SELECT network, account, conversation, id FROM message
                  WHERE pending = 1 AND sent < ?1",
            )?
            .query_map(params![before], |row| Ok((key_at(row)?, row.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut touched = Vec::new();
        for (conversation, id) in stuck {
            if let Some(mut message) = self.message(&conversation, &id)? {
                message.state = SendState::Failed {
                    reason: reason.to_owned(),
                };
                self.ingest(&conversation, &message, false)?;
            }
            if !touched.contains(&conversation) {
                touched.push(conversation);
            }
        }
        Ok(touched)
    }

    // People.

    /// People seen: authors, friends, search results. A person's relationship is left alone.
    pub fn upsert_people(&mut self, account: &AccountKey, people: &[PersonRow]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        for person in people {
            tx.execute(
                "INSERT INTO person (network, account, id, name, handle, avatar, bot, status, link)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (network, account, id) DO UPDATE SET
                     name = excluded.name, handle = excluded.handle, avatar = excluded.avatar,
                     bot = excluded.bot, status = excluded.status,
                     link = CASE WHEN excluded.link = '' THEN link ELSE excluded.link END",
                params![
                    word(&account.network),
                    account.account,
                    person.id,
                    person.name,
                    person.handle,
                    text(&person.avatar),
                    person.bot,
                    person.status,
                    person.link,
                ],
            )?;
        }
        tx.commit()
    }

    /// One relationship changing. The person must have been recorded.
    pub fn set_relation(&mut self, person: &Key, relation: Relation) -> rusqlite::Result<bool> {
        let changed = self.db.execute(
            "UPDATE person SET relation = ?4
              WHERE network = ?1 AND account = ?2 AND id = ?3 AND relation != ?4",
            params![word(&person.network), person.account, person.id, word(&relation)],
        )?;
        Ok(changed == 1)
    }

    /// The account's whole owned list, as the network sent it at connect. Anyone not in it is no
    /// longer a friend or contact.
    pub fn replace_relations(&mut self, account: &AccountKey, relations: &[(String, Relation)]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        let (network, id) = (word(&account.network), &account.account);
        tx.execute(
            "UPDATE person SET relation = 'none' WHERE network = ?1 AND account = ?2 AND relation != 'none'",
            params![network, id],
        )?;
        for (person, relation) in relations {
            tx.execute(
                "UPDATE person SET relation = ?4 WHERE network = ?1 AND account = ?2 AND id = ?3",
                params![network, id, person, word(relation)],
            )?;
        }
        tx.commit()
    }

    pub fn person(&self, person: &Key) -> rusqlite::Result<Option<Person>> {
        self.db
            .query_row(
                &format!("{PERSON} WHERE p.network = ?1 AND p.account = ?2 AND p.id = ?3"),
                params![word(&person.network), person.account, person.id],
                person_from,
            )
            .optional()
    }

    /// The owned lists, A to Z. With a query: everyone cached whose name or handle starts with it.
    pub fn people(
        &self,
        network: Option<Network>,
        relation: Option<Relation>,
        query: Option<&str>,
        offset: u32,
        limit: u32,
    ) -> rusqlite::Result<(Vec<Person>, u32)> {
        let matching = "(?1 IS NULL OR p.network = ?1) AND NOT p.bot
             AND CASE WHEN ?2 IS NOT NULL THEN p.relation = ?2
                      WHEN ?3 IS NULL THEN p.relation NOT IN ('none', 'blocked')
                      ELSE 1 END
             AND (?3 IS NULL OR p.name LIKE ?3 ESCAPE '\\' OR p.handle LIKE ?3 ESCAPE '\\')";
        let network = network.map(|network| word(&network));
        let relation = relation.map(|relation| word(&relation));
        let pattern = query.map(like_prefix);
        let total = self.db.query_row(
            &format!("SELECT COUNT(*) FROM person p WHERE {matching}"),
            params![network, relation, pattern],
            |row| row.get(0),
        )?;
        let people = self
            .db
            .prepare(&format!(
                "{PERSON} WHERE {matching}
                 ORDER BY p.name COLLATE NOCASE, p.network, p.account, p.id LIMIT ?4 OFFSET ?5"
            ))?
            .query_map(params![network, relation, pattern, limit, offset], person_from)?
            .collect::<rusqlite::Result<_>>()?;
        Ok((people, total))
    }

    /// What a person's row says beyond `PersonRow`: their title, time zone and tag.
    pub fn set_details(&mut self, account: &AccountKey, people: &[(String, PersonDetails)]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        for (id, details) in people {
            let mut title = details.title.clone();
            clip(&mut title, 120);
            tx.execute(
                "UPDATE person SET title = ?4, utc_offset = ?5, mark = ?6
                  WHERE network = ?1 AND account = ?2 AND id = ?3",
                params![word(&account.network), account.account, id, title, details.utc_offset, mark_word(&details.mark)],
            )?;
        }
        tx.commit()
    }

    /// People on the lists the user owns (friends, contacts), bots never, A to Z, at most
    /// `limit`: who could be around.
    pub fn owned(&self, network: Option<Network>, limit: u32) -> rusqlite::Result<Vec<Person>> {
        self.db
            .prepare(&format!(
                "{PERSON} WHERE (?1 IS NULL OR p.network = ?1) AND NOT p.bot AND p.relation IN ('friend', 'contact')
                 ORDER BY p.name COLLATE NOCASE, p.network, p.account, p.id LIMIT ?2"
            ))?
            .query_map(params![network.map(|network| word(&network)), limit], person_from)?
            .collect()
    }

    /// The people of direct conversations with a message since `since`, bots never, A to Z.
    pub fn talked_to(&self, network: Option<Network>, since: i64, limit: u32) -> rusqlite::Result<Vec<Person>> {
        self.db
            .prepare(&format!(
                "{PERSON} WHERE (?1 IS NULL OR p.network = ?1) AND NOT p.bot AND EXISTS
                     (SELECT 1 FROM conversation c
                       WHERE c.network = p.network AND c.account = p.account AND c.peer = p.id
                         AND c.kind = 'direct' AND c.updated >= ?2 AND NOT c.bot)
                 ORDER BY p.name COLLATE NOCASE, p.network, p.account, p.id LIMIT ?3"
            ))?
            .query_map(params![network.map(|network| word(&network)), since, limit], person_from)?
            .collect()
    }

    /// Requests from others waiting on me.
    pub fn incoming(&self, network: Option<Network>) -> rusqlite::Result<u32> {
        self.db.query_row(
            "SELECT COUNT(*) FROM person p WHERE (?1 IS NULL OR p.network = ?1) AND p.relation = 'incoming'",
            params![network.map(|network| word(&network))],
            |row| row.get(0),
        )
    }

    // Media.

    /// Files the network can serve and where from. A file already on disk stays there; a fresh
    /// address replaces an expired one, and is worth one more try after a failure.
    pub fn register_media(&mut self, account: &AccountKey, files: &[(MediaFile, String)]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        for (file, url) in files {
            tx.execute(
                "INSERT INTO media (network, account, id, kind, width, height, bytes, url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (network, account, id) DO UPDATE SET
                     failed = CASE WHEN url != excluded.url AND size = 0 THEN NULL ELSE failed END,
                     url = CASE WHEN size = 0 THEN excluded.url ELSE url END,
                     width = max(width, excluded.width), height = max(height, excluded.height),
                     bytes = CASE WHEN path IS NULL THEN excluded.bytes ELSE bytes END",
                params![word(&account.network), account.account, file.id, word(&file.kind), file.width, file.height, file.bytes as i64, url],
            )?;
        }
        tx.commit()
    }

    /// What is known about these files, and where each is fetched from. Unknown ids are left out.
    pub fn media(&self, account: &AccountKey, ids: &[String]) -> rusqlite::Result<Vec<(MediaFile, String)>> {
        let mut query = self.db.prepare(
            "SELECT id, kind, width, height, bytes, path, failed, url FROM media
              WHERE network = ?1 AND account = ?2 AND id = ?3",
        )?;
        let mut files = Vec::with_capacity(ids.len());
        for id in ids {
            let found = query
                .query_row(params![word(&account.network), account.account, id], |row| {
                    Ok((file_at(row, 0)?, row.get(7)?))
                })
                .optional()?;
            if let Some((Some(file), url)) = found {
                files.push((file, url));
            }
        }
        Ok(files)
    }

    /// The file is on disk at `path`. `None` when no such file was registered.
    pub fn media_landed(
        &mut self,
        account: &AccountKey,
        id: &str,
        path: &str,
        bytes: u64,
        now: i64,
    ) -> rusqlite::Result<Option<MediaFile>> {
        self.db.execute(
            "UPDATE media SET path = ?4, bytes = ?5, used = ?6, failed = NULL
              WHERE network = ?1 AND account = ?2 AND id = ?3",
            params![word(&account.network), account.account, id, path, bytes as i64, now],
        )?;
        Ok(self.media(account, &[id.to_owned()])?.pop().map(|(file, _)| file))
    }

    /// The fetch failed, and why in plain words. Until `media_retry`, or a fresh address from
    /// the adapter, the file is answered with this reason and not fetched. `None` when no such
    /// file was registered.
    pub fn media_failed(&mut self, account: &AccountKey, id: &str, reason: &str) -> rusqlite::Result<Option<MediaFile>> {
        self.db.execute(
            "UPDATE media SET failed = ?4 WHERE network = ?1 AND account = ?2 AND id = ?3 AND path IS NULL",
            params![word(&account.network), account.account, id, reason],
        )?;
        Ok(self.media(account, &[id.to_owned()])?.pop().map(|(file, _)| file))
    }

    /// The user asked for these again: their failures are forgotten. Answers how many had one.
    pub fn media_retry(&mut self, account: &AccountKey, ids: &[String]) -> rusqlite::Result<usize> {
        let tx = self.db.transaction()?;
        let mut cleared = 0;
        for id in ids {
            cleared += tx.execute(
                "UPDATE media SET failed = NULL
                  WHERE network = ?1 AND account = ?2 AND id = ?3 AND failed IS NOT NULL",
                params![word(&account.network), account.account, id],
            )?;
        }
        tx.commit()?;
        Ok(cleared)
    }

    /// The adapter exchanged an expired address for a fresh one (`Command::Download`): keep it,
    /// so the next fetch does not have to ask again.
    pub fn media_refreshed(&mut self, account: &AccountKey, id: &str, url: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE media SET url = ?4 WHERE network = ?1 AND account = ?2 AND id = ?3",
            params![word(&account.network), account.account, id, url],
        )?;
        Ok(())
    }

    /// The same file at other sizes: the longest side in pixels and where that size is fetched
    /// from, at most [`VARIANTS`]. Replaces what was registered for the file before. The file
    /// itself must have been registered; its own address is the size its `width` and `height`
    /// say.
    pub fn register_variants(&mut self, account: &AccountKey, id: &str, variants: &[(u32, String)]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        let key = params![word(&account.network), account.account, id];
        tx.execute("DELETE FROM media_variant WHERE network = ?1 AND account = ?2 AND id = ?3", key)?;
        for (size, url) in variants.iter().take(VARIANTS) {
            tx.execute(
                "INSERT OR REPLACE INTO media_variant (network, account, id, size, url) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![word(&account.network), account.account, id, size, url],
            )?;
            // The size already chosen keeps its place and takes the fresh address.
            tx.execute(
                "UPDATE media SET url = ?5 WHERE network = ?1 AND account = ?2 AND id = ?3 AND size = ?4",
                params![word(&account.network), account.account, id, size, url],
            )?;
        }
        tx.commit()
    }

    /// The shell will draw these files `size` pixels on their longest side. Each one that has a
    /// registered variant better than what it now points at is pointed at the smallest variant
    /// that is large enough (the largest there is, failing that) and loses its path, so the
    /// next fetch brings the larger one. A file never goes back to a smaller size. Answers the
    /// ids that changed.
    pub fn media_sized(&mut self, account: &AccountKey, ids: &[String], size: u32) -> rusqlite::Result<Vec<String>> {
        let tx = self.db.transaction()?;
        let mut changed = Vec::new();
        for id in ids {
            let key = params![word(&account.network), account.account, id];
            let held: Option<u32> = tx
                .query_row(
                    "SELECT CASE WHEN size > 0 THEN size ELSE max(width, height) END FROM media
                      WHERE network = ?1 AND account = ?2 AND id = ?3",
                    key,
                    |row| row.get(0),
                )
                .optional()?;
            let Some(held) = held.filter(|held| *held < size) else {
                continue;
            };
            let better: Option<(u32, String)> = tx
                .query_row(
                    "SELECT size, url FROM media_variant
                      WHERE network = ?1 AND account = ?2 AND id = ?3 AND size > ?4
                      ORDER BY size < ?5, abs(size - ?5) LIMIT 1",
                    params![word(&account.network), account.account, id, held, size],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((better, url)) = better {
                tx.execute(
                    "UPDATE media SET url = ?4, size = ?5, path = NULL, failed = NULL
                      WHERE network = ?1 AND account = ?2 AND id = ?3",
                    params![word(&account.network), account.account, id, url, better],
                )?;
                changed.push(id.clone());
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// The files were asked for again, so they are the last to be evicted.
    pub fn media_used(&mut self, account: &AccountKey, ids: &[String], now: i64) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE media SET used = ?4
                  WHERE network = ?1 AND account = ?2 AND id = ?3 AND path IS NOT NULL",
                params![word(&account.network), account.account, id, now],
            )?;
        }
        tx.commit()
    }

    /// Bytes on disk across every account.
    pub fn media_bytes(&self) -> rusqlite::Result<u64> {
        let bytes: i64 = self.db.query_row(
            "SELECT COALESCE(SUM(bytes), 0) FROM media WHERE path IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(bytes as u64)
    }

    /// Files on disk, least recently used first. With an account: that account's only.
    pub fn media_on_disk(&self, account: Option<&AccountKey>, limit: u32) -> rusqlite::Result<Vec<OnDisk>> {
        let network = account.map(|account| word(&account.network));
        let id = account.map(|account| account.account.as_str());
        let mut query = self.db.prepare(
            "SELECT network, account, id, path, bytes FROM media
              WHERE path IS NOT NULL AND (?1 IS NULL OR (network = ?1 AND account = ?2))
              ORDER BY used LIMIT ?3",
        )?;
        let rows = query.query_map(params![network, id, limit], |row| {
            Ok(OnDisk {
                account: AccountKey {
                    network: from_word(row.get(0)?)?,
                    account: row.get(1)?,
                },
                id: row.get(2)?,
                path: row.get(3)?,
                bytes: row.get::<_, i64>(4)? as u64,
            })
        })?;
        rows.collect()
    }

    /// The file was deleted from disk. The row stays so it can be fetched again.
    pub fn media_evicted(&mut self, account: &AccountKey, id: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE media SET path = NULL WHERE network = ?1 AND account = ?2 AND id = ?3",
            params![word(&account.network), account.account, id],
        )?;
        Ok(())
    }

    // Adapter notes.

    /// A user or channel name an adapter looked up once and should not ask for again.
    pub fn name(&self, account: &AccountKey, id: &str) -> rusqlite::Result<Option<String>> {
        self.db
            .query_row(
                "SELECT value FROM name WHERE network = ?1 AND account = ?2 AND id = ?3",
                params![word(&account.network), account.account, id],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn set_name(&mut self, account: &AccountKey, id: &str, value: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO name (network, account, id, value) VALUES (?1, ?2, ?3, ?4)",
            params![word(&account.network), account.account, id, value],
        )?;
        Ok(())
    }

    // Retention.

    /// Keeps the newest [`KEEP_MESSAGES`] of every conversation, and nothing older than `days`
    /// (P4, [`KEEP_DAYS`] by default) unless the conversation was opened in that time. Run at
    /// start, daily, and when P4 changes. Unsent messages are the user's and are never pruned.
    /// Returns how many rows went.
    pub fn prune(&mut self, now: i64, days: u32) -> rusqlite::Result<usize> {
        let cutoff = now - i64::from(days) * 86_400;
        let tx = self.db.transaction()?;
        let mut gone = tx.execute(
            "DELETE FROM message WHERE pending = 0 AND sent < ?1 AND NOT EXISTS
                 (SELECT 1 FROM conversation c
                   WHERE c.network = message.network AND c.account = message.account
                     AND c.id = message.conversation AND c.opened >= ?1)",
            params![cutoff],
        )?;
        let crowded: Vec<Key> = tx
            .prepare(
                "SELECT network, account, conversation FROM message
                  GROUP BY network, account, conversation HAVING COUNT(*) > ?1",
            )?
            .query_map(params![KEEP_MESSAGES], key_at)?
            .collect::<rusqlite::Result<_>>()?;
        for conversation in crowded {
            gone += tx.execute(
                &format!(
                    "DELETE FROM message
                      WHERE network = ?1 AND account = ?2 AND conversation = ?3 AND pending = 0
                        AND id NOT IN (SELECT id FROM message
                              WHERE network = ?1 AND account = ?2 AND conversation = ?3
                              {NEWEST_FIRST} LIMIT ?4)"
                ),
                params![word(&conversation.network), conversation.account, conversation.id, KEEP_MESSAGES],
            )?;
        }
        // A thread goes with its first message, unless the user opened it lately.
        let aged = "t.parent != '' AND t.root != '' AND t.opened < ?1 AND NOT EXISTS
                (SELECT 1 FROM message r
                  WHERE r.network = t.network AND r.account = t.account
                    AND r.conversation = t.parent AND r.id = t.root)";
        gone += tx.execute(
            &format!(
                "DELETE FROM message WHERE EXISTS
                     (SELECT 1 FROM conversation t
                       WHERE t.network = message.network AND t.account = message.account
                         AND t.id = message.conversation AND {aged})"
            ),
            params![cutoff],
        )?;
        tx.execute(&format!("DELETE FROM conversation AS t WHERE {aged}"), params![cutoff])?;
        // And one whose parent is gone altogether goes whatever its age.
        let orphaned = "t.parent != '' AND NOT EXISTS
                (SELECT 1 FROM conversation p
                  WHERE p.network = t.network AND p.account = t.account AND p.id = t.parent)";
        gone += tx.execute(
            &format!(
                "DELETE FROM message WHERE EXISTS
                     (SELECT 1 FROM conversation t
                       WHERE t.network = message.network AND t.account = message.account
                         AND t.id = message.conversation AND {orphaned})"
            ),
            [],
        )?;
        tx.execute(&format!("DELETE FROM conversation AS t WHERE {orphaned}"), [])?;
        tx.commit()?;
        Ok(gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Badge;

    const NOW: i64 = 1_800_000_000;

    fn store() -> (Store, Key) {
        let mut store = Store { db: Connection::open_in_memory().unwrap() };
        store.db.execute_batch(TABLES).unwrap();
        store.db.execute_batch(SETTINGS).unwrap();
        let account = AccountKey { network: Network::Discord, account: "me".into() };
        let row = ConversationRow::new("general", ConversationKind::Channel, "#general");
        store.upsert_conversation(&account, &row).unwrap();
        (store, Key::new(&account, "general"))
    }

    fn said(id: &str, text: &str, sent: i64) -> Message {
        Message::plain(id, "mara", text, sent, false)
    }

    fn ids(page: &MessagePage) -> Vec<&str> {
        page.messages.iter().map(|message| message.id.as_str()).collect()
    }

    #[test]
    fn pages_walk_a_conversation_both_ways_and_say_when_there_is_more() {
        let (mut store, key) = store();
        // Ids of two lengths in one second: they must still read in numeric order.
        for (id, sent) in [("8", 10), ("9", 20), ("10", 20), ("11", 20), ("12", 30)] {
            store.ingest(&key, &said(id, id, sent), false).unwrap();
        }
        let page = |store: &mut Store, page: Page, limit| store.messages(&key, &page, limit, NOW).unwrap().unwrap();

        let newest = page(&mut store, Page::Newest, 2);
        assert_eq!((ids(&newest), newest.older, newest.newer), (vec!["11", "12"], true, false));

        let before = page(&mut store, Page::Before { id: "11".into() }, 2);
        assert_eq!((ids(&before), before.older, before.newer), (vec!["9", "10"], true, true));
        let first = page(&mut store, Page::Before { id: "9".into() }, 2);
        assert_eq!((ids(&first), first.older), (vec!["8"], false));

        let after = page(&mut store, Page::After { id: "9".into() }, 5);
        assert_eq!((ids(&after), after.older, after.newer), (vec!["10", "11", "12"], true, false));

        let around = page(&mut store, Page::Around { id: "10".into() }, 2);
        assert_eq!((ids(&around), around.older, around.newer), (vec!["9", "10", "11"], true, true));

        let missing = Key { id: "nowhere".into(), ..key.clone() };
        assert!(store.messages(&missing, &Page::Newest, 2, NOW).unwrap().is_none());
    }

    #[test]
    fn live_messages_count_and_mentions_count_apart_and_reading_clears_both() {
        let (mut store, key) = store();
        let mut mention = said("2", "@you look", 20);
        mention.mentions_me = true;
        store.ingest(&key, &said("1", "hello", 10), true).unwrap();
        store.ingest(&key, &mention, true).unwrap();
        // History does not count, and neither does hearing the same message twice.
        store.ingest(&key, &said("0", "earlier", 5), false).unwrap();
        assert_eq!(store.ingest(&key, &mention, true).unwrap(), Stored::Same);

        let row = store.conversation(&key).unwrap().unwrap();
        assert_eq!((row.unread, row.mentions, row.preview.as_str()), (2, 1, "@you look"));
        assert_eq!(row.badge(), Badge::Count(1));
        assert_eq!(store.accounts().unwrap(), []);

        store.set_account(&key.account(), "me", "42").unwrap();
        let account = &store.accounts().unwrap()[0];
        assert_eq!((account.attention, account.unread), (1, false));

        assert!(store.mark_read(&key, "2").unwrap());
        assert!(!store.mark_read(&key, "2").unwrap(), "reading twice is not a change");
        let row = store.conversation(&key).unwrap().unwrap();
        assert_eq!((row.unread, row.mentions, row.last_read.as_deref()), (0, 0, Some("2")));
    }

    #[test]
    fn the_inbox_holds_what_needs_me_and_leaves_plain_channels_to_their_space() {
        let (mut store, key) = store();
        let account = key.account();
        let direct = ConversationRow::new("dm", ConversationKind::Direct, "Mara");
        let quiet = ConversationRow::new("quiet", ConversationKind::Channel, "#quiet");
        store.upsert_conversation(&account, &direct).unwrap();
        store.upsert_conversation(&account, &quiet).unwrap();
        store.ingest(&Key::new(&account, "dm"), &said("1", "hi", 10), true).unwrap();
        store.ingest(&Key::new(&account, "quiet"), &said("1", "noise", 30), true).unwrap();
        let mut mention = said("1", "@you", 20);
        mention.mentions_me = true;
        store.ingest(&key, &mention, true).unwrap();

        let titles = |store: &Store, filter| -> Vec<String> {
            let (rows, total) = store.inbox(None, filter, 0, 10).unwrap();
            assert_eq!(rows.len() as u32, total);
            rows.into_iter().map(|row| row.title).collect()
        };
        assert_eq!(titles(&store, InboxFilter::All), ["#general", "Mara"]);

        store.set_starred(&Key::new(&account, "quiet"), true).unwrap();
        assert_eq!(titles(&store, InboxFilter::All), ["#quiet", "#general", "Mara"]);
        // A direct message nobody mentioned me in is unread, not a mention.
        assert_eq!(titles(&store, InboxFilter::Mentions), ["#general"]);
        // Unread is a triage list: mentions first, then newest.
        assert_eq!(titles(&store, InboxFilter::Unread), ["#general", "#quiet", "Mara"]);
        assert_eq!(titles(&store, InboxFilter::Directs), ["Mara"]);

        store.set_muted(&Key::new(&account, "dm"), true).unwrap();
        assert_eq!(titles(&store, InboxFilter::Unread), ["#general", "#quiet"]);
        let counts = store.inbox_counts(None).unwrap();
        assert_eq!((counts.all, counts.unread, counts.mentions), (3, 2, 1));
        assert_eq!(store.inbox(Some(Network::Slack), InboxFilter::All, 0, 10).unwrap().1, 0);
    }

    #[test]
    fn the_inbox_leaves_out_the_old_the_empty_the_archived_and_requests_and_folds_apps() {
        let (mut store, key) = store();
        let account = key.account();
        let now = crate::hub::now();
        let day = 86_400;
        let row = |id: &str, kind, title: &str| ConversationRow::new(id, kind, title);
        let rows = [
            row("old", ConversationKind::Direct, "Old friend"),
            ConversationRow { filing: Some(Filing { pin: Some(0), ..Filing::default() }), ..row("starred-old", ConversationKind::Direct, "Starred old") },
            row("empty", ConversationKind::Direct, "Never spoke"),
            ConversationRow { bot: true, ..row("app", ConversationKind::Direct, "GitHub") },
            ConversationRow { filing: Some(Filing { archived: true, ..Filing::default() }), ..row("archived", ConversationKind::Group, "Archived") },
            ConversationRow { special: Special::Request, ..row("stranger", ConversationKind::Direct, "Stranger") },
            ConversationRow { filing: Some(Filing { hidden: true, ..Filing::default() }), ..row("hidden", ConversationKind::Group, "Hidden") },
            row("recent", ConversationKind::Direct, "Recent"),
        ];
        for row in &rows {
            store.upsert_conversation(&account, row).unwrap();
        }
        let at = |id: &str| Key::new(&account, id);
        for (id, days) in [("old", 40), ("starred-old", 40), ("app", 1), ("archived", 1), ("stranger", 1), ("hidden", 1), ("recent", 2)] {
            store.ingest(&at(id), &said(&format!("{id}-1"), "hello", now - days * day), true).unwrap();
        }
        store.set_starred(&at("starred-old"), true).unwrap();
        store.mark_read(&at("old"), "old-1").unwrap();
        let titles = |store: &Store, filter| -> Vec<String> {
            store.inbox(None, filter, 0, 20).unwrap().0.into_iter().map(|row| row.title).collect()
        };
        // Old, empty, archived, hidden and request rows are out; a starred old one stays; the
        // app is in, flagged, for the shell to fold.
        assert_eq!(titles(&store, InboxFilter::All), ["Starred old", "GitHub", "Recent"]);
        let counts = store.inbox_counts(None).unwrap();
        // The app's unread raises no count but its own dot; the archived one and the request
        // count for nothing.
        assert_eq!((counts.all, counts.unread, counts.apps), (3, 2, 1), "{counts:?}");
        store.set_account(&account, "Robin", "").unwrap();
        let attention: u32 = store.accounts().unwrap().iter().map(|row| row.attention).sum();
        assert_eq!(attention, 2, "the starred and the recent direct message, nothing else");

        // A mention brings the archived chat in; a channel that mentioned me stays a day after
        // it was read.
        let mut mention = said("archived-2", "@you", now);
        mention.mentions_me = true;
        store.ingest(&at("archived"), &mention, true).unwrap();
        let mut ping = said("ping", "@you", now);
        ping.mentions_me = true;
        store.ingest(&key, &ping, true).unwrap();
        store.mark_read(&key, "ping").unwrap();
        assert_eq!(titles(&store, InboxFilter::All), ["Starred old", "Archived", "#general", "GitHub", "Recent"]);
        // Kept a day only: the channel's mention is a day and a minute old.
        store.db.execute("UPDATE conversation SET noticed = ?1 WHERE id = 'general'", params![now - day - 60]).unwrap();
        assert!(!titles(&store, InboxFilter::All).contains(&"#general".to_owned()));
    }

    #[test]
    fn a_network_home_lists_its_pins_then_recency_and_a_folder_is_a_filter() {
        let (mut store, key) = store();
        let account = key.account();
        let now = crate::hub::now();
        let chat = |id: &str, folders: &[&str], pin: Option<i32>, archived: bool| ConversationRow {
            filing: Some(Filing { folders: folders.iter().map(|id| (*id).to_owned()).collect(), pin, archived, ..Filing::default() }),
            ..ConversationRow::new(id, ConversationKind::Direct, id)
        };
        for row in [
            chat("ana", &["work", "family"], None, false),
            chat("ben", &["work"], Some(1), false),
            chat("cy", &[], Some(0), false),
            chat("dee", &["family"], None, true),
            ConversationRow { special: Special::Request, ..ConversationRow::new("eve", ConversationKind::Direct, "eve") },
        ] {
            store.upsert_conversation(&account, &row).unwrap();
        }
        // The fixture's channel belongs to a Space, as every channel of a Space does.
        store.db.execute("UPDATE conversation SET space = 'guild' WHERE id = 'general'", []).unwrap();
        for (at, id) in ["dee", "cy", "ben", "ana", "eve"].iter().enumerate() {
            store.ingest(&Key::new(&account, *id), &said(&format!("{id}-1"), "hi", now - 100 + at as i64), true).unwrap();
        }
        store
            .replace_folders(&account, &[
                FolderRow { id: "work".into(), name: "Work".into(), position: 0, pinned: vec!["ben".into()], ..FolderRow::default() },
                FolderRow { id: "family".into(), name: "Family".into(), position: 1, pinned: vec!["dee".into()], ..FolderRow::default() },
            ])
            .unwrap();
        let ids = |store: &Store, folder: Option<&str>, shelf| -> Vec<String> {
            let (rows, total, _) = store.home(Network::Discord, None, folder, shelf, 0, 20).unwrap();
            assert_eq!(rows.len() as u32, total);
            rows.into_iter().map(|row| row.key.id).collect()
        };
        // Pins in their order, then newest first; the channel of the Space is not in it.
        assert_eq!(ids(&store, None, Shelf::Main), ["cy", "ben", "ana"]);
        assert_eq!(ids(&store, None, Shelf::Archive), ["dee"]);
        assert_eq!(ids(&store, None, Shelf::Requests), ["eve"]);
        // A chat in two folders is in both; a folder's own pins come first, archived or not.
        assert_eq!(ids(&store, Some("work"), Shelf::Main), ["ben", "ana"]);
        assert_eq!(ids(&store, Some("family"), Shelf::Main), ["dee", "ana"]);
        let folders = store.folders(Some(Network::Discord)).unwrap();
        let counted: Vec<(String, u32)> = folders.into_iter().map(|folder| (folder.name, folder.unread)).collect();
        assert_eq!(counted, [("Work".to_owned(), 2), ("Family".to_owned(), 2)]);

        // A tree reload that does not know the filing leaves it; one that does replaces it.
        store.upsert_conversation(&account, &ConversationRow::new("ben", ConversationKind::Direct, "ben")).unwrap();
        assert_eq!(ids(&store, Some("work"), Shelf::Main), ["ben", "ana"]);
        assert!(store.set_filing(&Key::new(&account, "ben"), &Filing::default()).unwrap());
        assert_eq!(ids(&store, Some("work"), Shelf::Main), ["ana"]);
    }

    #[test]
    fn a_row_says_who_spoke_last_and_whether_they_read_mine() {
        let (mut store, key) = store();
        store.ingest(&key, &said("10", "ok", 100), true).unwrap();
        let row = store.conversation(&key).unwrap().unwrap();
        assert_eq!((row.preview.as_str(), row.preview_from.as_str(), row.preview_mine), ("ok", "mara", false));

        let mut mine = Message::plain("11", "me", "", 200, true);
        mine.attachments = (0..2).map(|at| protocol::Attachment::new(format!("p{at}.png"), MediaFile::new(format!("p{at}"), MediaKind::Image))).collect();
        store.ingest(&key, &mine, false).unwrap();
        let row = store.conversation(&key).unwrap().unwrap();
        assert_eq!((row.preview.as_str(), row.preview_from.as_str(), row.preview_mine, row.seen), ("2 photos", "", true, false));
        // Read up to an older message of mine is not read up to this one.
        assert!(store.set_seen(&key, "9").unwrap());
        assert!(!store.conversation(&key).unwrap().unwrap().seen);
        assert!(store.set_seen(&key, "11").unwrap());
        assert!(store.conversation(&key).unwrap().unwrap().seen);

        // A line from a list that carries only that, never older than what is shown.
        let line = |id: &str, sent| PreviewLine { id: id.into(), text: "from the list".into(), sender: "Ana".into(), mine: false, sent };
        assert!(!store.set_preview(&key, &line("5", 150)).unwrap());
        assert!(store.set_preview(&key, &line("12", 300)).unwrap());
        let row = store.conversation(&key).unwrap().unwrap();
        assert_eq!((row.preview.as_str(), row.preview_from.as_str(), row.updated), ("from the list", "Ana", 300));
        assert_eq!(store.previewless(std::slice::from_ref(&key)).unwrap(), Vec::<Key>::new());
    }

    #[test]
    fn the_network_s_copy_replaces_the_pending_one_and_a_stuck_one_fails() {
        let (mut store, key) = store();
        let mut pending = Message::plain("c-1", "me", "on it", 100, true);
        pending.state = SendState::Pending;
        pending.client_id = Some("c-1".into());
        store.ingest(&key, &pending, false).unwrap();
        assert_eq!(store.newest(&key).unwrap(), None, "a pending message is not on the network");

        let mut echoed = Message::plain("777", "me", "on it", 101, true);
        echoed.client_id = Some("c-1".into());
        assert_eq!(store.ingest(&key, &echoed, true).unwrap(), Stored::New);
        let page = store.messages(&key, &Page::Newest, 10, NOW).unwrap().unwrap();
        assert_eq!(ids(&page), ["777"]);
        assert_eq!(page.messages[0].state, SendState::Sent);

        let mut stuck = pending.clone();
        stuck.id = "c-2".into();
        stuck.client_id = Some("c-2".into());
        store.ingest(&key, &stuck, false).unwrap();
        assert_eq!(store.fail_pending(100, "no answer").unwrap(), [], "not old enough yet");
        assert_eq!(store.fail_pending(101, "no answer").unwrap(), std::slice::from_ref(&key));
        let failed = store.message(&key, "c-2").unwrap().unwrap();
        assert_eq!(failed.state, SendState::Failed { reason: "no answer".into() });
        assert_eq!(store.fail_pending(i64::MAX, "no answer").unwrap(), []);
    }

    #[test]
    fn a_replaced_tree_drops_what_left_and_keeps_what_the_user_did() {
        let (mut store, key) = store();
        let account = key.account();
        let space = Key::new(&account, "guild");
        let spaces = [SpaceRow { id: "guild".into(), name: "Rust".into(), icon: None, position: 0 }];
        store.replace_spaces(&account, &spaces).unwrap();
        let group = |id: &str| Group::new(id, id, 0);
        let channel = |id: &str, position| ConversationRow {
            position,
            group: Some("text".into()),
            ..ConversationRow::new(id, ConversationKind::Channel, format!("#{id}"))
        };
        store.replace_tree(&space, &[group("text")], &[channel("b", 2), channel("a", 1)]).unwrap();
        store.ingest(&Key::new(&account, "a"), &said("1", "hi", 10), true).unwrap();
        store.ingest(&Key::new(&account, "b"), &said("1", "yo", 10), true).unwrap();
        assert!(store.set_collapsed(&space, "text", true).unwrap());

        store.replace_tree(&space, &[group("text"), group("voice")], &[channel("a", 1), channel("c", 3)]).unwrap();
        let Tree { groups, conversations: rows, total } = store.tree(&space, 0, 10).unwrap().unwrap();
        let listed: Vec<_> = rows.iter().map(|row| (row.key.id.as_str(), row.unread)).collect();
        assert_eq!((listed, total), (vec![("a", 1), ("c", 0)], 2));
        assert_eq!(groups.iter().map(|g| (g.id.as_str(), g.collapsed)).collect::<Vec<_>>(), [("text", true), ("voice", false)]);
        assert!(store.messages(&Key::new(&account, "b"), &Page::Newest, 10, NOW).unwrap().is_none());
        // The conversation outside the Space was not the tree's to remove.
        assert!(store.conversation(&key).unwrap().is_some());
        let space_row = &store.spaces(Some(Network::Discord)).unwrap()[0];
        assert_eq!((space_row.attention, space_row.unread), (0, true));
        assert!(store.tree(&Key::new(&account, "elsewhere"), 0, 10).unwrap().is_none());
    }

    #[test]
    fn reactions_are_counted_per_emoji_and_a_file_gains_its_path_when_it_lands() {
        let (mut store, key) = store();
        let account = key.account();
        let mut message = said("1", "look", 10);
        let thumb = MediaFile { width: 800, height: 600, ..MediaFile::new("att-1", protocol::MediaKind::Image) };
        message.attachments.push(protocol::Attachment {
            thumb: Some(thumb.clone()),
            mime: "image/png".into(),
            ..protocol::Attachment::new("shot.png", MediaFile::new("att-1-full", protocol::MediaKind::Image))
        });
        store.register_media(&account, &[(thumb, "https://cdn.example/att-1".into())]).unwrap();
        store.ingest(&key, &message, false).unwrap();

        let thumbs_up = |mine| Reaction { emoji: "👍".into(), file: None, count: 1, mine };
        assert!(store.react(&key, "1", &thumbs_up(false), true).unwrap());
        assert!(store.react(&key, "1", &thumbs_up(true), true).unwrap());
        assert!(store.react(&key, "1", &thumbs_up(false), false).unwrap());
        assert!(!store.react(&key, "1", &Reaction { emoji: "🎉".into(), ..thumbs_up(false) }, false).unwrap());

        assert_eq!(store.media_bytes().unwrap(), 0);
        let landed = store.media_landed(&account, "att-1", "/tmp/media/att-1", 4_096, NOW).unwrap().unwrap();
        assert_eq!((landed.width, landed.bytes), (800, 4_096));
        let page = store.messages(&key, &Page::Newest, 10, NOW).unwrap().unwrap();
        let shown = &page.messages[0];
        assert_eq!(shown.reactions, [thumbs_up(true)]);
        let attachment = &shown.attachments[0];
        assert_eq!(attachment.thumb.as_ref().unwrap().path.as_deref(), Some("/tmp/media/att-1"));
        assert_eq!(attachment.file.path, None, "the full file was never fetched");
        assert_eq!(store.media_on_disk(Some(&account), 10).unwrap().len(), 1);

        store.forget(&account).unwrap();
        assert!(store.conversation(&key).unwrap().is_none());
        assert_eq!(store.media_bytes().unwrap(), 0);
    }

    #[test]
    fn pruning_keeps_the_newest_and_what_was_opened_lately() {
        let (mut store, key) = store();
        let account = key.account();
        let old = NOW - (KEEP_DAYS + 1) * 86_400;
        let opened = Key::new(&account, "opened");
        store.upsert_conversation(&account, &ConversationRow::new("opened", ConversationKind::Direct, "Mara")).unwrap();
        store.ingest(&key, &said("1", "ancient", old), false).unwrap();
        store.ingest(&key, &said("2", "recent", NOW - 60), false).unwrap();
        store.ingest(&opened, &said("1", "ancient but read lately", old), false).unwrap();
        store.messages(&opened, &Page::Newest, 1, NOW - 3_600).unwrap();

        assert_eq!(store.prune(NOW, KEEP_DAYS as u32).unwrap(), 1);
        assert!(store.message(&key, "1").unwrap().is_none());
        assert!(store.message(&key, "2").unwrap().is_some());
        assert!(store.message(&opened, "1").unwrap().is_some());
    }

    #[test]
    fn a_shorter_retention_is_counted_first_and_then_deletes_just_that() {
        let (mut store, key) = store();
        store.ingest(&key, &said("1", "five weeks old", NOW - 35 * 86_400), false).unwrap();
        store.ingest(&key, &said("2", "ten days old", NOW - 10 * 86_400), false).unwrap();
        store.ingest(&key, &said("3", "today", NOW - 60), false).unwrap();
        assert_eq!(store.prunable(NOW, 90).unwrap().0, 0);
        let (messages, bytes) = store.prunable(NOW, 7).unwrap();
        assert_eq!(messages, 2);
        assert!(bytes > 0);
        assert_eq!(store.prunable(NOW, 30).unwrap().0, 1);
        assert_eq!(store.prune(NOW, 30).unwrap(), 1);
        assert_eq!(store.prune(NOW, 7).unwrap(), 1);
        assert!(store.message(&key, "3").unwrap().is_some());
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tc-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("chat.sqlite")
    }

    #[test]
    fn settings_outlive_a_schema_change_and_sign_out_takes_only_that_accounts() {
        use protocol::settings::{account_scope, conversation_scope, VERSION};
        let path = scratch("settings");
        let mut store = Store::open(&path).unwrap();
        let (slack, discord) = (
            AccountKey { network: Network::Slack, account: "T1".into() },
            AccountKey { network: Network::Discord, account: "D1".into() },
        );
        store.set_setting("", "keep_days", Some("30")).unwrap();
        store.set_setting(&account_scope(&slack), "typing", Some("off")).unwrap();
        store.set_setting(&conversation_scope(&Key::new(&slack, "C1")), "sound", Some("chime")).unwrap();
        store.set_setting(&account_scope(&discord), "typing", Some("on")).unwrap();
        store.set_setting("", "keep_days", Some("7")).unwrap();
        let read = |store: &Store| -> Vec<(String, String, String)> {
            store.settings().unwrap().into_iter().map(|s| (s.scope, s.key, s.value)).collect()
        };
        assert!(read(&store).contains(&(String::new(), "keep_days".into(), "7".into())), "a second write replaces the first");
        assert!(read(&store).contains(&(String::new(), VERSION.into(), "1".into())));

        // A schema change drops the cache and keeps the user's choices.
        store.db.execute_batch("PRAGMA user_version = 1;").unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert_eq!(read(&store).len(), 5);

        store.forget(&slack).unwrap();
        let left = read(&store);
        assert!(left.iter().all(|(scope, _, _)| !scope.contains(":slack:")), "{left:?}");
        assert!(left.iter().any(|(scope, _, _)| *scope == account_scope(&discord)));

        store.set_setting("", "keep_days", None).unwrap();
        assert!(!read(&store).iter().any(|(_, key, _)| key == "keep_days"), "reset to the default is no row");
        store.reset_settings().unwrap();
        assert_eq!(read(&store), [(String::new(), VERSION.to_owned(), "1".to_owned())]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn clearing_the_cache_takes_messages_and_files_and_leaves_drafts_and_rows() {
        let path = scratch("clear");
        let mut store = Store::open(&path).unwrap();
        let account = AccountKey { network: Network::Discord, account: "me".into() };
        store.upsert_conversation(&account, &ConversationRow::new("general", ConversationKind::Channel, "#general")).unwrap();
        let key = Key::new(&account, "general");
        store.ingest(&key, &said("1", "hello", NOW), false).unwrap();
        store.set_draft(&key, "half a thought").unwrap();
        store.clear_cache().unwrap();
        assert!(store.message(&key, "1").unwrap().is_none());
        let row = store.conversation(&key).unwrap().expect("the conversation stays");
        assert_eq!(row.draft, "half a thought");
        assert_eq!(store.media_bytes().unwrap(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
