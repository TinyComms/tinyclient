//! One adapter per account. Each speaks its network's own protocol and nothing else.
//!
//! This file is the whole surface between an adapter and the rest of chatd:
//!
//! - [`Command`] is everything chatd asks of an adapter. An adapter serves the ones it can and
//!   hands the rest to [`Command::unsupported`], so a new command never breaks an adapter.
//! - [`Context`] is everything an adapter tells chatd. Each call writes the store and announces
//!   the right change; an adapter never touches the store's tables or the change list itself.
//! - An adapter's module exports `start`, `accounts` and `without` (see the functions of those
//!   names at the bottom), and reads its addresses through [`net::address`] so a test can point
//!   it at a local fake.
//!
//! Adapters hold bounded state: trees, people and messages go to the store as they arrive.
//!
//! Nothing here acts for the user unasked. A friend request is answered, a presence set, a
//! message edited, deleted or reacted to only by the command the shell's request became.

mod discord;
pub mod net;
pub mod signal;
mod slack;
mod teams;
pub mod telegram;

use crate::hub::{lock, now, Hub};
use crate::session;
use crate::store::{
    ConversationRow, Filing, FolderRow, PersonDetails, PersonRow, PreviewLine, SpaceRow, Stored,
    KEEP_MESSAGES,
};
use protocol::{
    AccountKey, Activity, CallFailure, CallState, Capabilities, CatalogItem, CatalogKind, Change,
    Contact, Group, Key, Link, MediaFile, MemberSection, Message, Network, Notify, Occupant,
    Occupants, Outgoing, Participant, Person, Presence, Reaction, RelateAction, Relation,
    SendState, SharedItem, SharedKind, Typist, VoiceJoin, Warning, Watching,
};
use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Why an adapter did not do what a command asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// This adapter does not do this (yet).
    Unsupported,
    /// The network said no, in words the user can read: "Slow mode: 8 s".
    Refused(String),
}

/// Where the answer to a command goes. Send exactly one.
pub type Reply<T> = Sender<Result<T, Refusal>>;

/// How to fetch a file: the address to ask now, and the headers the session adds.
#[derive(Debug, Clone)]
pub struct Download {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// One window of a member list.
#[derive(Debug, Clone)]
pub struct Members {
    pub sections: Vec<MemberSection>,
    pub members: Vec<Person>,
}

/// What a person's card shows beyond their row, as the network tells it.
#[derive(Debug, Clone, Default)]
pub struct About {
    pub bio: String,
    /// Titles of the Spaces we share.
    pub mutual_spaces: Vec<String>,
    /// As the network names it: `Europe/Lisbon`.
    pub time_zone: String,
    /// Seconds east of UTC now.
    pub utc_offset: Option<i32>,
    /// Whatever else the network says of them, label and value, in its own order: Phone,
    /// Username, Email, Title, Pronouns. At most twelve are kept.
    pub fields: Vec<(String, String)>,
    /// Unix seconds they joined the network.
    pub since: Option<i64>,
    pub mutual_friends: u32,
    /// Their banner's colour, `0xRRGGBB`, and its picture as a registered file.
    pub accent: Option<u32>,
    pub banner: Option<protocol::MediaFile>,
    // Typed. Fill these rather than `fields`: chatd moves a `fields` line labelled Pronouns,
    // Title, Department, Email, Phone or Roles into its place, and keeps the rest as leftovers.
    /// Their nickname in the Space asked about (`Command::ProfileIn`).
    pub nickname: String,
    pub pronouns: String,
    pub title: String,
    pub department: String,
    /// Role names in that Space, colours left out.
    pub roles: Vec<String>,
    /// Email and phone; chatd masks phone numbers.
    pub contacts: Vec<Contact>,
    /// Mutual Spaces in all, when more than `mutual_spaces` lists.
    pub mutual_total: u32,
    /// Unix seconds: joined that Space, became friends, last seen.
    pub joined: Option<i64>,
    pub friends_since: Option<i64>,
    pub last_seen: Option<i64>,
    pub status_until: Option<i64>,
    /// Out of office, in the network's words, and until when.
    pub away: String,
    pub away_until: Option<i64>,
    pub note: String,
    pub warning: Option<Warning>,
}

/// What a conversation's info pane says beyond its row, as the network tells it.
#[derive(Debug, Clone, Default)]
pub struct ConversationAbout {
    pub description: String,
    pub members: Option<u32>,
    pub online: Option<u32>,
    pub created: Option<i64>,
    pub creator: String,
    pub notify: Notify,
    pub pinned: Option<u32>,
    pub external: String,
    pub link: String,
}

/// What a Space's info says, as the network tells it.
#[derive(Debug, Clone, Default)]
pub struct SpaceAbout {
    pub description: String,
    pub members: Option<u32>,
    pub online: Option<u32>,
    pub nickname: String,
    pub roles: Vec<String>,
    pub notify: Notify,
}

/// A page of what was shared, from the network's own search.
#[derive(Debug, Clone, Default)]
pub struct SharedPage {
    pub items: Vec<SharedItem>,
    /// The network has more below the last one.
    pub more: bool,
    /// The exact total, where the network counted.
    pub total: Option<u32>,
}

/// A page of a picker's catalog, and where the next one starts.
#[derive(Debug, Clone, Default)]
pub struct CatalogPage {
    pub items: Vec<CatalogItem>,
    pub cursor: Option<String>,
}

/// What chatd asks of an adapter. Ids are the network's own; the account is the adapter's.
pub enum Command {
    /// Fetch the recent history of a conversation the user opened, and correct what is cached
    /// (edits and deletes made while offline).
    Backfill(String),
    /// The same fetch for a conversation nobody is waiting on. Runs after everything else.
    Warm(String),
    /// The user scrolled past the oldest cached message: fetch the page before it.
    History { conversation_id: String, before: String },
    /// The user is reading forward from `after` through history that is not the newest page:
    /// fetch the page after it if the cache has a gap there. Sent for every such page, so check
    /// what is stored first (`Context::stored_ids`) and ask the network only for what is missing.
    HistoryAfter { conversation_id: String, after: String },
    /// The user jumped to a message (a reply's original, a search hit, a link): fetch the page
    /// around it. Sent when the cache does not hold the message or runs out on either side.
    HistoryAround { conversation_id: String, around: String },
    /// The user opened a Space: bring its tree up to date if the network needs asking.
    OpenSpace(String),
    /// Tell the network the conversation has been read up to this message.
    MarkRead { conversation_id: String, message_id: String },
    /// Send, with `client_id` as the network's nonce. Always answer through [`Context::sent`].
    Send {
        conversation_id: String,
        client_id: String,
        text: String,
        reply_to: Option<String>,
    },
    /// The same with files or options (`Outgoing`): stream each file from its path in parts,
    /// never the whole file in memory, telling [`Context::upload`] as it goes. Always answer
    /// through [`Context::sent`]; a failure names the file.
    SendWith {
        conversation_id: String,
        client_id: String,
        text: String,
        reply_to: Option<String>,
        with: Box<Outgoing>,
    },
    /// Stop uploading a `SendWith` and answer it through [`Context::sent`] as failed
    /// ("Cancelled"). Nothing, if it already went.
    CancelSend { conversation_id: String, client_id: String },
    /// Add (`on`) or take back our reaction. `emoji` is the emoji itself, or the id of a custom
    /// emoji's registered file. The store already shows it; answer `Err` and it is taken back.
    /// The network's own event for it is reported as usual and counts once.
    React {
        conversation_id: String,
        message_id: String,
        emoji: String,
        on: bool,
        reply: Reply<()>,
    },
    /// Replace the text of one of our messages. Report the edited message through
    /// [`Context::message`] before answering: nothing was changed locally.
    Edit {
        conversation_id: String,
        message_id: String,
        text: String,
        reply: Reply<()>,
    },
    /// Delete a message for everyone. Report it through [`Context::deleted`] before answering.
    Delete {
        conversation_id: String,
        message_id: String,
        reply: Reply<()>,
    },
    /// The user's draft is changing. Pace it: at most one signal per 8 s per conversation.
    Typing(String),
    /// What the shell has on screen changed. Subscribe to presence, typing and member ranges for
    /// exactly this, and drop what is no longer in it.
    Watch(Arc<Watching>),
    /// Search the network's directory. Results go to [`Context::people`]; at most 25.
    SearchPeople(String),
    Members {
        conversation_id: String,
        offset: u32,
        limit: u32,
        reply: Reply<Members>,
    },
    /// The id of the direct conversation with a person, created if there is none.
    StartDirect { person_id: String, reply: Reply<String> },
    /// One person's card. One request to the network, and only because the user opened it.
    Profile { person_id: String, reply: Reply<About> },
    /// The same, opened from inside a Space: add the nickname, roles and join date there. An
    /// adapter that answers `Unsupported` is asked `Profile` instead.
    ProfileIn { person_id: String, space_id: String, reply: Reply<About> },
    /// A conversation's info pane: one request, because the user opened it.
    ConversationInfo { conversation_id: String, reply: Reply<ConversationAbout> },
    /// A Space's info, opened from its name.
    SpaceInfo { space_id: String, reply: Reply<SpaceAbout> },
    /// Set the network's notification setting. Report the new mute through [`Context::muted`]
    /// before answering.
    SetMute { conversation_id: String, notify: Notify, reply: Reply<()> },
    /// Leave a group conversation. Report it gone through [`Context::conversation_gone`].
    Leave { conversation_id: String, reply: Reply<()> },
    /// Close a direct conversation as the network's own × does. Report it through
    /// [`Context::filing`] (hidden) or [`Context::conversation_gone`].
    CloseDirect { conversation_id: String, reply: Reply<()> },
    /// The messages pinned in a conversation, newest first, at most 50.
    Pinned { conversation_id: String, reply: Reply<Vec<Message>> },
    /// What was shared in a conversation, from the network's own search, because the user
    /// clicked "Search all". `before` continues below an item already listed.
    SearchShared {
        conversation_id: String,
        kind: SharedKind,
        before: Option<String>,
        limit: u32,
        reply: Reply<SharedPage>,
    },
    /// A picker's catalog: custom emoji (of one Space when `space_id` is set), stickers, or GIFs
    /// for `query`. Register every preview through [`Context::media`]. Only on a key press:
    /// never ahead.
    Catalog {
        kind: CatalogKind,
        space_id: Option<String>,
        query: String,
        cursor: Option<String>,
        reply: Reply<CatalogPage>,
    },
    /// These rows are on screen with no preview line. Fetch each one's newest message, at most
    /// one request a second, and record it ([`Context::history`] or [`Context::preview`]).
    /// Never sent to Discord, where a fetch per DM is the burst that flags accounts.
    Previews(Vec<String>),
    /// The user opened the network's home list (`Request::Home`): bring the conversations that
    /// belong to no Space up to date if the network needs asking.
    OpenHome,
    /// The user opened a Space's Threads list: fetch the network's list of followed threads
    /// once, and record them with `Filing::followed`.
    OpenThreads(String),
    /// The user answered, withdrew or ended a friendship, or blocked someone. Report the new
    /// relationship through [`Context::relation`] before answering.
    Relate {
        person_id: String,
        action: RelateAction,
        reply: Reply<()>,
    },
    /// The user set their own presence, and their status text unless it is `None` (empty clears
    /// it). The hub already shows it; answer `Err` and it is put back.
    SetPresence {
        presence: Presence,
        status: Option<String>,
        reply: Reply<()>,
    },
    /// A registered file is wanted: answer with where and how to fetch it now.
    Download { id: String, url: String, reply: Reply<Download> },
    /// The link that opens this conversation's call in the official app.
    Handoff { conversation_id: String, reply: Reply<String> },
    /// mediad is up: join or ring on the network, then report through [`Context::voice`].
    CallJoin(String),
    /// Leave, cancel or decline on the network. Also sent when the call failed here (mediad
    /// died, a deadline passed), so the network stops showing us in the channel or ringing the
    /// other side. May arrive for a call the adapter already left: then it is nothing.
    CallLeave(String),
    /// Our mute and deafen changed; the network shows them to others.
    CallAudio { muted: bool, deafened: bool },
    /// The user signed out: end the session on the network, best effort, then answer. chatd
    /// waits five seconds for the answer and forgets the account either way.
    SignOut(Reply<()>),
    /// The adapter is being dropped. `serve_commands` returns on it; an adapter never sees it.
    Stop,
}

impl Command {
    /// Answers a command this adapter does not serve, so nothing waits for it.
    pub fn unsupported(self, ctx: &Context) {
        match self {
            Command::Members { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StartDirect { reply, .. } | Command::Handoff { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::Profile { reply, .. } | Command::ProfileIn { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ConversationInfo { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SpaceInfo { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetMute { reply, .. } | Command::Leave { reply, .. } | Command::CloseDirect { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::Pinned { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SearchShared { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Catalog { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SendWith { conversation_id, client_id, .. } => {
                let why = format!("Sending files and options is not built for {} yet.", ctx.account.network.label());
                ctx.sent(&conversation_id, &client_id, Err(why));
            }
            Command::React { reply, .. }
            | Command::Edit { reply, .. }
            | Command::Delete { reply, .. }
            | Command::Relate { reply, .. }
            | Command::SetPresence { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            // Nothing to end on the network: the session is simply forgotten.
            Command::SignOut(reply) => drop(reply.send(Ok(()))),
            Command::Download { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Send { conversation_id, client_id, .. } => {
                ctx.sent(&conversation_id, &client_id, Err("Sending is not built for this network.".to_owned()));
            }
            Command::CallJoin(conversation_id) => {
                let detail = format!("{} calls are not built yet.", ctx.account.network.label());
                ctx.call_failed(&conversation_id, CallFailure::Refused, &detail);
            }
            _ => {}
        }
    }
}

/// Why a connection ended.
pub enum Failure {
    /// The network rejected the session. Retrying cannot help; the user has to sign in again.
    SignedOut(String),
    /// Anything else. Reconnect after a pause.
    Dropped(String),
}

impl<E: std::fmt::Display> From<E> for Failure {
    fn from(err: E) -> Failure {
        Failure::Dropped(err.to_string())
    }
}

/// Tells an adapter's threads to stop, and wakes the ones that are waiting.
#[derive(Default)]
struct Halt {
    stopped: AtomicBool,
    /// Panics caught on this adapter's threads. The first is recovered from; the second stops
    /// the adapter and the account says why.
    panics: AtomicU32,
    waiting: Mutex<()>,
    wake: Condvar,
}

/// What every adapter thread holds, and every call an adapter makes back into chatd.
#[derive(Clone)]
pub struct Context {
    pub account: AccountKey,
    hub: Arc<Hub>,
    /// The adapter's own inbox, so its socket thread can queue work for its command thread.
    pub commands: Sender<Command>,
    stop: Arc<Halt>,
}

type Stores = rusqlite::Result<()>;

impl Context {
    pub fn stopped(&self) -> bool {
        self.stop.stopped.load(Ordering::Relaxed)
    }

    /// Waits for `time`, or less if the adapter is stopped meanwhile. True if it was stopped.
    /// One wakeup however long the wait: use this, not a loop of short sleeps.
    pub fn pause(&self, time: Duration) -> bool {
        let waiting = lock(&self.stop.waiting);
        drop(self.stop.wake.wait_timeout_while(waiting, time, |_| !self.stopped()));
        self.stopped()
    }

    pub fn key(&self, id: &str) -> Key {
        Key::new(&self.account, id)
    }

    // The account.

    /// The socket is up (`Connected`) or down (`Offline`).
    pub fn link(&self, link: Link) {
        if link != Link::Connected {
            self.hub.forget_live(&self.account);
        }
        self.hub.set_link(&self.account, link, "");
    }

    /// Something only the network's own app can do stands in the way: a captcha, a phone or
    /// e-mail verification. Say what in words the user can act on; empty once it is done. Shown
    /// on the account whatever its link is.
    pub fn attention(&self, required: &str) {
        self.hub.set_required(&self.account, required);
    }

    /// The network reissued part of the session (Slack's rotated `d` cookie). `rewrite` is given
    /// the network's whole stored session, which may hold other accounts, and answers it with
    /// the new part in place; `None` leaves it. True once the keychain holds it.
    pub fn save_session(&self, rewrite: impl FnOnce(&str) -> Option<String>) -> bool {
        match session::rewrite(self.account.network, rewrite) {
            Ok(saved) => saved,
            Err(why) => {
                eprintln!("chatd: {}: the session was not saved: {}", self.account.network.label(), net::redact(&why));
                false
            }
        }
    }

    /// The account's name (a workspace's, or the user's own) and our person id on it.
    pub fn describe(&self, name: &str, me: &str) -> Stores {
        self.hub.store().set_account(&self.account, name, me)?;
        self.hub.changed(Change::Status);
        Ok(())
    }

    // Spaces, trees and rows.

    /// Every Space of the account, as the network lists them at connect.
    pub fn spaces(&self, spaces: &[SpaceRow]) -> Stores {
        self.hub.store().replace_spaces(&self.account, spaces)?;
        self.hub.changed(Change::Spaces { account: self.account.clone() });
        Ok(())
    }

    /// One Space's whole tree: only what the user may see, in the network's order. Rows go
    /// straight to the store; keep none of them.
    pub fn tree(&self, space_id: &str, groups: &[Group], conversations: &[ConversationRow]) -> Stores {
        let space = self.key(space_id);
        self.hub.store().replace_tree(&space, groups, conversations)?;
        self.hub.changed(Change::Tree { space });
        Ok(())
    }

    /// One group of a Space created, renamed or moved, without sending the whole tree again.
    pub fn group(&self, space_id: &str, group: &Group) -> Stores {
        let space = self.key(space_id);
        self.hub.store().upsert_group(&space, group)?;
        self.hub.changed(Change::Tree { space });
        Ok(())
    }

    /// A group was removed. Its conversations stay, under no group, until the network moves them.
    pub fn group_gone(&self, space_id: &str, group_id: &str) -> Stores {
        let space = self.key(space_id);
        if self.hub.store().remove_group(&space, group_id)? {
            self.hub.changed(Change::Tree { space });
        }
        Ok(())
    }

    /// Everyone in a voice channel, as the network lists them, us included when we are in it,
    /// and how many there are in all. Report it for every voice channel of a Space on screen
    /// whenever it changes; an empty list when the channel empties. Never stored, and the tree
    /// is not announced: the row is told alone.
    pub fn occupants(&self, conversation_id: &str, people: Vec<Occupant>, total: u32) {
        self.hub.set_occupants(self.key(conversation_id), Occupants { people, total });
    }

    /// One conversation: a direct message, or a channel created, renamed or moved.
    pub fn conversation(&self, row: &ConversationRow) -> Stores {
        self.hub.store().upsert_conversation(&self.account, row)?;
        self.row_changed(&row.id, row.space.as_deref());
        if row.filing.is_some() {
            self.hub.changed(Change::Folders { account: self.account.clone() });
        }
        Ok(())
    }

    pub fn conversation_gone(&self, conversation_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        let space = self.hub.store().conversation(&conversation)?.and_then(|row| row.space);
        self.hub.store().remove_conversation(&conversation)?;
        self.row_changed(conversation_id, space.as_deref());
        self.hub.changed(Change::Status);
        Ok(())
    }

    /// The unread state as the network has it: at connect, and whenever another device reads.
    /// `unread` for a channel may be just 1 for "some". `updated` is the newest message's time.
    pub fn counts(
        &self,
        conversation_id: &str,
        unread: u32,
        mentions: u32,
        last_read: Option<&str>,
        updated: i64,
    ) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_counts(&conversation, unread, mentions, last_read, updated)? {
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    /// The network's own mute setting, after its rules (overrides, suppress flags) are applied.
    pub fn muted(&self, conversation_id: &str, muted: bool) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_muted(&conversation, muted)? {
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    /// Where the network files one conversation: its folders, archive, pin, hidden, followed.
    /// One call for one change; never a re-read of every row.
    pub fn filing(&self, conversation_id: &str, filing: &Filing) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_filing(&conversation, filing)? {
            let space = self.hub.store().conversation(&conversation)?.and_then(|row| row.space);
            self.row_changed(conversation_id, space.as_deref());
            self.hub.changed(Change::Status);
            self.hub.changed(Change::Folders { account: self.account.clone() });
        }
        Ok(())
    }

    /// Every folder of the account, in the network's order: Telegram's chat folders, Discord's
    /// server folders. Membership of chats is on their rows (`Filing::folders`).
    pub fn folders(&self, folders: &[FolderRow]) -> Stores {
        self.hub.store().replace_folders(&self.account, folders)?;
        self.hub.changed(Change::Folders { account: self.account.clone() });
        self.hub.changed(Change::Spaces { account: self.account.clone() });
        Ok(())
    }

    /// A Space's own settings: muted as a whole, and the user's "hide muted channels".
    pub fn space_flags(&self, space_id: &str, muted: bool, hide_muted: bool) -> Stores {
        let space = self.key(space_id);
        if self.hub.store().set_space_flags(&space, muted, hide_muted)? {
            self.hub.changed(Change::Spaces { account: self.account.clone() });
            self.hub.changed(Change::Tree { space });
        }
        Ok(())
    }

    /// The newest message as a line, where the network's list gives only that (Teams'
    /// `lastMessage`). Where a whole message is at hand, record it with [`Context::history`].
    pub fn preview(&self, conversation_id: &str, line: &PreviewLine) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_preview(&conversation, line)? {
            self.hub.changed(Change::Conversation { conversation });
        }
        Ok(())
    }

    /// The other side has read my messages up to this one (Telegram's ✓✓, Teams' "Seen").
    pub fn seen(&self, conversation_id: &str, message_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_seen(&conversation, message_id)? {
            self.hub.changed(Change::Conversation { conversation });
        }
        Ok(())
    }

    fn row_changed(&self, conversation_id: &str, space: Option<&str>) {
        self.hub.changed(Change::Conversation { conversation: self.key(conversation_id) });
        if let Some(space) = space {
            self.hub.changed(Change::Tree { space: self.key(space) });
        }
    }

    // Messages.

    /// Announces what recording messages did. `live` ones may also have changed the totals.
    fn stored(&self, conversation: &Key, stored: &[Stored], live: bool) {
        if stored.iter().any(|stored| *stored != Stored::Same) {
            self.hub.changed(Change::Messages { conversation: conversation.clone() });
        }
        if stored.contains(&Stored::New) {
            // A new message moves the row and changes its preview.
            self.hub.changed(Change::Conversation { conversation: conversation.clone() });
            if live {
                self.hub.changed(Change::Status);
            }
        }
    }

    /// A message heard on the socket: new, or the newer form of a stored one (an edit, a link
    /// preview). Its conversation must have been recorded. A new incoming one counts as unread;
    /// set `mentions_me` by the network's own rules, mute and suppress settings included. If it
    /// carries the `client_id` of a message we are sending, it replaces that one.
    pub fn message(&self, conversation_id: &str, message: &Message) -> Stores {
        let conversation = self.key(conversation_id);
        let stored = self.hub.store().ingest(&conversation, message, true)?;
        if stored == Stored::New && !message.outgoing {
            self.typing_stopped(conversation_id, &message.sender.id);
        }
        self.stored(&conversation, &[stored], true);
        Ok(())
    }

    /// A page of history, in any order. Does not count as unread: the network already said how
    /// much is. Messages already stored and unchanged cost nothing.
    pub fn history(&self, conversation_id: &str, messages: &[Message]) -> Stores {
        let conversation = self.key(conversation_id);
        let mut stored = Vec::with_capacity(messages.len());
        for message in messages {
            stored.push(self.hub.store().ingest(&conversation, message, false)?);
        }
        self.stored(&conversation, &stored, false);
        Ok(())
    }

    /// The message as stored, for networks whose updates are partial and must be merged.
    pub fn stored_message(&self, conversation_id: &str, message_id: &str) -> rusqlite::Result<Option<Message>> {
        self.hub.store().message(&self.key(conversation_id), message_id)
    }

    /// The ids stored between two message ids, both included, oldest first, at most `limit`: a
    /// page from the network is compared against this to find what was deleted while away.
    pub fn stored_ids(&self, conversation_id: &str, oldest: &str, newest: &str, limit: u32) -> rusqlite::Result<Vec<String>> {
        self.hub.store().ids_between(&self.key(conversation_id), oldest, newest, limit.min(KEEP_MESSAGES))
    }

    /// A message is gone. One with a thread hanging off it stays as a tombstone, and replies to
    /// it say their original was deleted.
    pub fn deleted(&self, conversation_id: &str, message_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().delete_message(&conversation, message_id)? {
            self.hub.changed(Change::Messages { conversation });
        }
        Ok(())
    }

    /// One person added or removed one reaction. Set `reaction.mine` when it was us.
    pub fn reacted(&self, conversation_id: &str, message_id: &str, reaction: &Reaction, added: bool) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().react(&conversation, message_id, reaction, added)? {
            self.hub.changed(Change::Messages { conversation });
        }
        Ok(())
    }

    /// The outcome of a `Send`: the message as the network accepted it, or why it did not go.
    pub fn sent(&self, conversation_id: &str, client_id: &str, outcome: Result<Message, String>) {
        let conversation = self.key(conversation_id);
        let stored = match outcome {
            Ok(mut message) => {
                message.client_id = Some(client_id.to_owned());
                self.hub.store().ingest(&conversation, &message, false)
            }
            Err(reason) => {
                let pending = self.hub.store().message(&conversation, client_id);
                match pending {
                    Ok(Some(mut message)) => {
                        message.state = SendState::Failed { reason };
                        self.hub.store().ingest(&conversation, &message, false)
                    }
                    // Discarded while it was on its way.
                    Ok(None) => Ok(Stored::Same),
                    Err(err) => Err(err),
                }
            }
        };
        match stored {
            Ok(stored) => self.stored(&conversation, &[stored], true),
            Err(err) => eprintln!("chatd: {}: {}", self.account.network.label(), net::redact(&err.to_string())),
        }
    }

    /// The id of the newest message stored for a conversation.
    pub fn newest(&self, conversation_id: &str) -> rusqlite::Result<Option<String>> {
        self.hub.store().newest(&self.key(conversation_id))
    }

    /// The row as stored, or `None` for a conversation never recorded.
    pub fn known(&self, conversation_id: &str) -> rusqlite::Result<Option<protocol::Conversation>> {
        self.hub.store().conversation(&self.key(conversation_id))
    }

    // People.

    /// People seen: friends, direct partners, authors, search results. Never a whole directory.
    pub fn people(&self, people: &[PersonRow]) -> Stores {
        self.hub.store().upsert_people(&self.account, people)?;
        self.hub.changed(Change::People { account: self.account.clone() });
        Ok(())
    }

    /// The whole list the user owns, by person id, as sent at connect. Record the people first.
    pub fn relations(&self, relations: &[(String, Relation)]) -> Stores {
        self.hub.store().replace_relations(&self.account, relations)?;
        self.hub.changed(Change::People { account: self.account.clone() });
        Ok(())
    }

    pub fn relation(&self, person_id: &str, relation: Relation) -> Stores {
        if self.hub.store().set_relation(&self.key(person_id), relation)? {
            self.hub.changed(Change::People { account: self.account.clone() });
        }
        Ok(())
    }

    /// What people's rows say beyond `PersonRow`: title, time zone, tag. Record the people
    /// first.
    pub fn details(&self, people: &[(String, PersonDetails)]) -> Stores {
        self.hub.store().set_details(&self.account, people)?;
        self.hub.changed(Change::People { account: self.account.clone() });
        Ok(())
    }

    /// Their status emoji and what they are doing (playing, in voice, in a meeting), kept like
    /// presence: never stored, and only for owned lists and people the shell watches.
    pub fn activity(&self, person_id: &str, emoji: &str, activity: Option<Activity>, owned: bool) {
        self.hub.set_activity(self.key(person_id), emoji, activity, owned);
    }

    /// What this account may send: file limits, pickers, options. Say it once the session
    /// tells (the plan, the workspace's settings); until then nothing is offered.
    pub fn capabilities(&self, can: Capabilities) {
        self.hub.set_capabilities(&self.account, can);
    }

    /// How far a `SendWith`'s files have gone, in bytes of all of them. Call as often as is
    /// convenient: the shell is told at most four times a second.
    pub fn upload(&self, conversation_id: &str, client_id: &str, sent: u64, total: u64) {
        self.hub.set_upload(self.key(conversation_id), client_id, sent, total);
    }

    /// The network finished a `SearchPeople`, whether or not it found anyone. Record what it
    /// found through [`Context::people`] first.
    pub fn searched(&self, query: &str) {
        self.hub.changed(Change::Searched {
            account: self.account.clone(),
            query: query.to_owned(),
        });
    }

    /// Rows of a conversation's member list were added, removed or moved.
    pub fn members_changed(&self, conversation_id: &str) {
        self.hub.changed(Change::Members { conversation: self.key(conversation_id) });
    }

    /// What is held about a person's presence now, for a member row built here.
    pub fn presence_of(&self, person_id: &str) -> Presence {
        self.hub.presence(&self.key(person_id))
    }

    /// Never stored. `owned` marks someone on a list the user owns; anyone else is kept only
    /// while the shell is watching them, so pass on whatever the network sends. Our own presence
    /// is reported the same way, under our own id and `owned`.
    pub fn presence(&self, person_id: &str, presence: Presence, status: &str, owned: bool) {
        self.hub.set_presence(self.key(person_id), presence, status, owned);
    }

    /// Someone is typing, for `seconds` unless more is heard (the network's own timeout).
    pub fn typing(&self, conversation_id: &str, person_id: &str, name: &str, seconds: i64) {
        let typist = Typist {
            id: person_id.to_owned(),
            name: name.to_owned(),
            until: now() + seconds,
        };
        self.hub.set_typing(self.key(conversation_id), typist, true);
    }

    pub fn typing_stopped(&self, conversation_id: &str, person_id: &str) {
        let typist = Typist {
            id: person_id.to_owned(),
            name: String::new(),
            until: 0,
        };
        self.hub.set_typing(self.key(conversation_id), typist, false);
    }

    /// What the shell has on screen, across every account. Also arrives as `Command::Watch`.
    pub fn watching(&self) -> Arc<Watching> {
        self.hub.watching()
    }

    // Media.

    /// Files the messages, rows and people recorded here refer to, each with the address it is
    /// fetched from. Register before recording what refers to them. Nothing is fetched until the
    /// shell asks.
    pub fn media(&self, files: &[(MediaFile, String)]) -> Stores {
        self.hub.store().register_media(&self.account, files)
    }

    /// The same picture at other sizes: the longest side in pixels, and where that size is
    /// fetched from. Register the file first, with the address and the `width` and `height` of
    /// the size a row normally shows; then the sizes the network can also serve, at most six
    /// (for a 2x screen at least twice the normal one). `Fetch` names the size the shell draws
    /// at and chatd fetches the smallest that is large enough.
    pub fn media_variants(&self, id: &str, variants: &[(u32, String)]) -> Stores {
        self.hub.store().register_variants(&self.account, id, variants)
    }

    /// A registered file's address ran out and was exchanged for this one (while serving
    /// `Command::Download`): keep it, so the next fetch need not ask again.
    pub fn media_refreshed(&self, id: &str, url: &str) -> Stores {
        self.hub.store().media_refreshed(&self.account, id, url)
    }

    // Calls. Signalling only; mediad carries the media.

    /// Someone is calling us in a conversation.
    pub fn ring(&self, conversation_id: &str, from: &str) {
        self.hub.calls.ring(&self.hub, self.key(conversation_id), from);
    }

    /// The caller gave up, or another device answered.
    pub fn ring_stopped(&self, conversation_id: &str) {
        self.hub.calls.ring_stopped(&self.hub, &self.key(conversation_id));
    }

    /// We are ringing the other side of a direct call and nobody has answered yet.
    pub fn ringing_out(&self, conversation_id: &str) {
        self.hub.calls.set_state(&self.hub, &self.key(conversation_id), CallState::RingingOut);
    }

    /// The other side of a call we rang picked up. Until this, a ring we placed is never shown
    /// as a call, whatever our own media is doing.
    pub fn answered(&self, conversation_id: &str) {
        self.hub.calls.answered(&self.hub, &self.key(conversation_id));
    }

    /// The network handed out the voice session for the call the user started, or a new one for
    /// a call that is already up (the voice server moved): mediad is told to join, or to rejoin.
    pub fn voice(&self, conversation_id: &str, voice: VoiceJoin) {
        self.hub.calls.join(&self.hub, &self.key(conversation_id), voice);
    }

    /// Everyone in the call but us, with their mute and deafen as the network shows them.
    pub fn participants(&self, conversation_id: &str, participants: Vec<Participant>) {
        self.hub.calls.set_participants(&self.hub, &self.key(conversation_id), participants);
    }

    pub fn call_failed(&self, conversation_id: &str, reason: CallFailure, detail: &str) {
        let failed = CallState::Failed {
            reason,
            detail: detail.to_owned(),
        };
        self.hub.calls.set_state(&self.hub, &self.key(conversation_id), failed);
    }

    // Notes.

    /// Something small the adapter looked up once and should not ask the network for again.
    pub fn note(&self, key: &str) -> Option<String> {
        self.hub.store().name(&self.account, key).ok().flatten()
    }

    pub fn set_note(&self, key: &str, value: &str) {
        let _ = self.hub.store().set_name(&self.account, key, value);
    }
}

/// A running adapter. Dropping it disconnects.
pub struct Adapter {
    commands: Sender<Command>,
    stop: Arc<Halt>,
}

impl Adapter {
    pub fn command(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// The adapter's inbox, for a thread that asks on its own (the media cache).
    pub fn sender(&self) -> Sender<Command> {
        self.commands.clone()
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        self.stop.stopped.store(true, Ordering::Relaxed);
        drop(self.stop.waiting.lock());
        self.stop.wake.notify_all();
        let _ = self.commands.send(Command::Stop);
    }
}

/// What the account says when tinyclient itself failed on it, and stopped trying.
pub const BROKEN: &str = "Something went wrong in tinyclient with this account. Quit and reopen tinyclient to try again.";
/// What the account says while it recovers from tinyclient's own failure.
pub const RECOVERING: &str = "Something went wrong in tinyclient with this account. Trying again.";
/// The pause before an adapter thread that panicked runs again.
const PANIC_PAUSE: Duration = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_secs(30) };
const PANIC_PAUSE_COMMANDS: Duration = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_secs(5) };

impl Context {
    /// A panic was caught on one of this adapter's threads. Answers true when it is the first
    /// and the thread should run again after its pause; the second stops the adapter, and the
    /// account is offline with [`BROKEN`] until the user restarts tinyclient.
    fn panicked(&self, thread: &str) -> bool {
        let count = self.stop.panics.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!("chatd: {}: the {thread} thread panicked ({count})", self.account.network.label());
        self.hub.forget_live(&self.account);
        if count > 1 {
            self.stop.stopped.store(true, Ordering::Relaxed);
            drop(self.stop.waiting.lock());
            self.stop.wake.notify_all();
            let _ = self.commands.send(Command::Stop);
            self.hub.set_link(&self.account, Link::Offline, BROKEN);
            return false;
        }
        true
    }
}

/// Counts recent drops of connections that had held, so a link that drops every minute backs
/// off as one that never held does, rather than signing on again every minute for ever. The
/// count halves every [`DROPS_HALF_LIFE`].
#[derive(Debug, Default)]
struct Drops {
    count: f64,
    last: Option<Instant>,
}

const DROPS_HALF_LIFE: Duration = Duration::from_secs(600);

impl Drops {
    /// One more drop at `now`: the pause before the next attempt. `held` says the connection
    /// was up longer than a minute; `pause` is the last pause, which a connection that never
    /// held doubles.
    fn next(&mut self, now: Instant, held: bool, pause: Duration) -> Duration {
        let since = self.last.map_or(Duration::MAX, |last| now.saturating_duration_since(last));
        let decay = 0.5f64.powf(since.as_secs_f64() / DROPS_HALF_LIFE.as_secs_f64());
        self.count = self.count * decay + 1.;
        self.last = Some(now);
        if !held {
            return (pause * 2).min(RETRY_CEILING);
        }
        // The first drop in a while retries in a second; each recent one doubles it.
        let doublings = (self.count.floor() as u32).saturating_sub(1).min(16);
        Duration::from_secs(1u64 << doublings).min(RETRY_CEILING)
    }
}

/// Keeps one connection up: reconnects with a growing pause, and gives up only when the network
/// says the session is no longer valid. Then the account is `Expired`: its session and cache
/// stay, so what was read stays readable and signing in again replaces the session.
///
/// A panic inside `connect` is caught here, at the thread's edge: the account goes offline
/// with [`RECOVERING`] and connects again after [`PANIC_PAUSE`]; a second panic on this
/// adapter stops it with [`BROKEN`].
fn keep_connected(ctx: &Context, mut connect: impl FnMut() -> Failure) {
    let mut pause = Duration::from_secs(1);
    let mut drops = Drops::default();
    while !ctx.stopped() {
        let began = Instant::now();
        let failure = match catch_unwind(AssertUnwindSafe(&mut connect)) {
            Ok(failure) => failure,
            Err(_) => {
                if !ctx.panicked("connection") {
                    return;
                }
                ctx.hub.set_link(&ctx.account, Link::Offline, RECOVERING);
                if ctx.pause(PANIC_PAUSE) {
                    return;
                }
                continue;
            }
        };
        if ctx.stopped() {
            return;
        }
        match failure {
            Failure::SignedOut(why) => {
                eprintln!("chatd: {}: signed out: {}", ctx.account.network.label(), net::redact(&why));
                ctx.hub.forget_live(&ctx.account);
                ctx.hub.set_link(&ctx.account, Link::Expired, &why);
                return;
            }
            Failure::Dropped(why) => {
                eprintln!("chatd: {}: {}", ctx.account.network.label(), net::redact(&why));
            }
        }
        // A connection that held for a while earns a quick retry, unless it has been dropping
        // often. One that never holds backs off to a quarter of an hour: unattended, that is
        // under a hundred sign-ons a day, not 1,400.
        pause = drops.next(Instant::now(), began.elapsed() > Duration::from_secs(60), pause);
        ctx.hub.forget_live(&ctx.account);
        ctx.hub.set_offline(&ctx.account, pause <= RECONNECTING);
        // Not on the second: many clients retrying in step is what a network watches for.
        let jitter = pause.mul_f64(0.25 * f64::from(now().unsigned_abs() as u32 % 1000) / 1000.);
        if ctx.pause(pause + jitter) {
            return;
        }
    }
}

/// A retry this close is "reconnecting"; one further off is plain offline.
const RECONNECTING: Duration = Duration::from_secs(30);

/// The longest pause between attempts to connect.
const RETRY_CEILING: Duration = Duration::from_secs(900);

/// How long a fetched history is trusted. The socket keeps it current in between.
const FRESH: Duration = Duration::from_secs(60);

/// Runs an adapter's commands until it is stopped. What the user is waiting on goes first.
/// `pace` is the pause after a history fetch, to stay inside the network's rate limit.
///
/// With nothing queued this waits on the inbox and nothing else: no timer, so an idle account
/// costs no wakeups. Dropping the [`Adapter`] sends `Command::Stop`, which ends it.
///
/// A panic while serving one command is caught: that command's reply is dropped (its asker
/// hears it failed), and the thread serves on after a short pause. A second panic on this
/// adapter stops it, and the account says why ([`Context::panicked`]).
pub(crate) fn serve_commands(
    ctx: &Context,
    inbox: Receiver<Command>,
    pace: Duration,
    mut backfill: impl FnMut(&str),
    mut other: impl FnMut(Command),
) {
    let mut urgent = VecDeque::new();
    let mut warm = VecDeque::new();
    let mut fetched: HashMap<String, Instant> = HashMap::new();
    while !ctx.stopped() {
        if urgent.is_empty() && warm.is_empty() {
            match inbox.recv() {
                Ok(Command::Stop) | Err(_) => return,
                Ok(Command::Warm(id)) => warm.push_back(id),
                Ok(command) => urgent.push_back(command),
            }
        }
        for command in inbox.try_iter() {
            match command {
                Command::Stop => return,
                Command::Warm(id) => warm.push_back(id),
                command => urgent.push_back(command),
            }
        }
        let id = match urgent.pop_front() {
            Some(Command::Backfill(id)) => id,
            Some(command) => {
                if catch_unwind(AssertUnwindSafe(|| other(command))).is_err() && (!ctx.panicked("command") || ctx.pause(PANIC_PAUSE_COMMANDS)) {
                    return;
                }
                continue;
            }
            None => match warm.pop_front() {
                Some(id) => id,
                None => continue,
            },
        };
        if fetched.get(&id).is_some_and(|at| at.elapsed() < FRESH) {
            continue;
        }
        if catch_unwind(AssertUnwindSafe(|| backfill(&id))).is_err() && (!ctx.panicked("command") || ctx.pause(PANIC_PAUSE_COMMANDS)) {
            return;
        }
        fetched.insert(id, Instant::now());
        if ctx.pause(pace) {
            return;
        }
    }
}

/// One account found in what a sign-in produced.
pub struct Signed {
    /// Stable across restarts and safe as a file name: letters, digits, `.`, `_` and `-`.
    pub account: String,
    /// Shown until the adapter connects and calls [`Context::describe`].
    pub name: String,
    /// What `start` is given for this account.
    pub session: String,
}

/// True for the networks `start` can run.
pub fn supported(network: Network) -> bool {
    match network {
        Network::Slack | Network::Discord | Network::Teams => true,
        Network::Telegram => telegram::supported(),
        Network::Signal => signal::supported(),
    }
}

/// An account id names a directory of the media cache, so it must be one plain file name:
/// letters, digits, `.`, `_` and `-`, and neither of the two names that mean a directory.
fn safe(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// The accounts inside one network's stored session. One Slack sign-in holds a workspace each.
pub fn accounts(network: Network, session: &str) -> Result<Vec<Signed>, String> {
    let found = match network {
        Network::Slack => slack::accounts(session)?,
        Network::Discord => discord::accounts(session)?,
        Network::Telegram => telegram::accounts(session)?,
        Network::Teams => teams::accounts(session)?,
        Network::Signal => signal::accounts(session)?,
    };
    match found.iter().find(|signed| !safe(&signed.account)) {
        Some(bad) => Err(format!("{}: unusable account id {:?}", network.label(), bad.account)),
        None => Ok(found),
    }
}

/// The stored session with one account taken out, for signing out of that account alone. `None`
/// when nothing would be left, and the keychain item goes.
pub fn without(network: Network, session: &str, account: &str) -> Option<String> {
    match network {
        Network::Slack => slack::without(session, account),
        Network::Discord => discord::without(session, account),
        Network::Telegram => telegram::without(session, account),
        Network::Teams => teams::without(session, account),
        Network::Signal => signal::without(session, account),
    }
}

/// Starts the adapter for an account from its part of the session.
pub fn start(account: AccountKey, session: &str, hub: Arc<Hub>) -> Result<Adapter, String> {
    let stop = Arc::new(Halt::default());
    let (commands, inbox): (Sender<Command>, Receiver<Command>) = channel();
    let ctx = Context {
        account: account.clone(),
        hub,
        commands: commands.clone(),
        stop: stop.clone(),
    };
    match account.network {
        Network::Slack => slack::start(ctx.clone(), session, inbox)?,
        Network::Discord => discord::start(ctx.clone(), session, inbox)?,
        Network::Telegram => telegram::start(ctx.clone(), session, inbox)?,
        Network::Teams => teams::start(ctx.clone(), session, inbox)?,
        Network::Signal => signal::start(ctx.clone(), session, inbox)?,
    }
    ctx.link(Link::Offline);
    Ok(Adapter { commands, stop })
}

/// An adapter with no network behind it, for whatever serves an account inside chatd (the
/// demo). The caller runs `serve_commands` on the inbox with the context.
pub(crate) fn local(account: AccountKey, hub: Arc<Hub>) -> (Adapter, Context, Receiver<Command>) {
    let stop = Arc::new(Halt::default());
    let (commands, inbox) = channel();
    let ctx = Context {
        account,
        hub,
        commands: commands.clone(),
        stop: stop.clone(),
    };
    (Adapter { commands, stop }, ctx, inbox)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn hub() -> Arc<Hub> {
        Arc::new(Hub::new(Store::open(std::path::Path::new(":memory:")).expect("a store in memory")))
    }

    fn linked(hub: &Hub, account: &AccountKey) -> (Link, String) {
        let links = hub.links();
        let linked = links.iter().find(|linked| linked.account == *account).expect("the account is linked");
        (linked.link, linked.detail.clone())
    }

    #[test]
    fn a_link_that_drops_every_minute_backs_off_and_one_drop_after_a_quiet_hour_does_not() {
        let t0 = Instant::now();
        let minute = Duration::from_secs(60);
        let mut drops = Drops::default();
        let mut pause = Duration::from_secs(1);
        let mut pauses = Vec::new();
        for n in 0..12 {
            pause = drops.next(t0 + minute * n, true, pause);
            pauses.push(pause.as_secs());
        }
        assert_eq!(pauses[..2], [1, 1], "the first drops retry at once");
        assert!(pauses.windows(2).all(|pair| pair[1] >= pair[0]), "{pauses:?}");
        assert!(pauses[11] >= 120, "after twelve drops in twelve minutes it waits minutes: {pauses:?}");
        // An hour of quiet: the count has decayed, and a drop retries in a second again.
        let quiet = drops.next(t0 + minute * 80, true, pause);
        assert!(quiet <= Duration::from_secs(2), "{quiet:?}");
        // One that never held doubles, up to the ceiling, whatever the count.
        assert_eq!(drops.next(t0 + minute * 81, false, Duration::from_secs(8)), Duration::from_secs(16));
        assert_eq!(drops.next(t0 + minute * 82, false, RETRY_CEILING), RETRY_CEILING);
    }

    #[test]
    fn a_panicking_command_is_recovered_from_once_and_the_second_stops_the_account_with_words() {
        let hub = hub();
        let account = AccountKey { network: Network::Slack, account: "T1".into() };
        let (adapter, ctx, inbox) = local(account.clone(), hub.clone());
        ctx.link(Link::Connected);
        let served = Arc::new(Mutex::new(Vec::new()));
        let seen = served.clone();
        let thread_ctx = ctx.clone();
        let thread = std::thread::spawn(move || {
            serve_commands(&thread_ctx, inbox, Duration::ZERO, |id| {
                if id.starts_with("bad") {
                    panic!("a bug in the adapter");
                }
                lock(&seen).push(id.to_owned());
            }, |_| {})
        });
        adapter.command(Command::Backfill("bad1".into()));
        adapter.command(Command::Backfill("good".into()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&served).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*lock(&served), ["good"], "the thread served on after the first panic");
        assert_eq!(linked(&hub, &account), (Link::Connected, String::new()), "one recovered panic leaves the link alone");
        adapter.command(Command::Backfill("bad2".into()));
        thread.join().expect("the panic was caught, not carried out of the thread");
        assert!(ctx.stopped());
        assert_eq!(linked(&hub, &account), (Link::Offline, BROKEN.to_owned()));
    }

    #[test]
    fn a_panicking_connection_says_why_and_is_connected_again_once() {
        let hub = hub();
        let account = AccountKey { network: Network::Discord, account: "1".into() };
        let (_adapter, ctx, _inbox) = local(account.clone(), hub.clone());
        let mut attempts = 0;
        let mut said = None;
        let watch = (hub.clone(), account.clone());
        keep_connected(&ctx, || {
            attempts += 1;
            if attempts == 1 {
                panic!("a bug in the socket thread");
            }
            said = Some(linked(&watch.0, &watch.1));
            Failure::SignedOut("done".into())
        });
        assert_eq!(attempts, 2, "restarted once");
        assert_eq!(said, Some((Link::Offline, RECOVERING.to_owned())), "while it recovered the account said why");
        assert_eq!(linked(&hub, &account).0, Link::Expired);
    }
}
