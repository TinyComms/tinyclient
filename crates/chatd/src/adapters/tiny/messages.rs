//! Text channels and direct messages on a Tiny server. Voice rooms stay voice channels.
//!
//! The shell already draws a conversation once a row is in the store, so this module only
//! maps the server's JSON into that store and answers the commands the shell already sends.
//! A GIF or a video is an attachment the existing viewer fetches. Reactions are Unicode emoji.

use ::e2ee as e2ee_core;
use super::e2ee::{self, Read};
use super::live::{Ask, Live};
use super::Saved;
use crate::blobs::upload::{self, Target};
use crate::store::{ConversationRow, PersonRow, PreviewLine, SpaceRow};
use crate::adapters::{CatalogPage, Command, Context, Download, Refusal};
use protocol::{
    join_host, Attachment, CallSupport, Capabilities, CatalogKind, ConversationKind, Group, GroupKind,
    Encryption, MediaFile, MediaKind, Message, Outgoing, Reaction, ReactionRules, ReplyTo, Seal,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// One space per server. The account already makes it unique.
const SPACE: &str = "server";
const PAGE: u32 = 50;
const PAGE_MAX: u32 = 100;
const DIRECTS_AT: i32 = 500_000;
const ROOMS_AT: i32 = 1_000_000;
const TYPING_FOR: i64 = 8;

/// Lists, the space, and what this account may send. Names come from `GET /v1/members`.
pub(crate) fn refresh(ctx: &Context, saved: &Saved) {
    publish(ctx, saved, &member_names(saved));
}

/// The same as [`refresh`], with names the caller already loaded (a fresh `ready` has them).
pub(crate) fn publish(ctx: &Context, saved: &Saved, names: &HashMap<String, String>) {
    ctx.capabilities(account_caps(saved));
    let _ = ctx.spaces(&[SpaceRow {
        id: SPACE.to_owned(),
        name: join_host(&saved.base_url),
        icon: None,
        position: 0,
    }]);
    let Some(channels) = optional_list::<WireChannel>(saved, "/v1/channels", "channels") else {
        return;
    };
    let Some(directs) = optional_list::<WireDirect>(saved, "/v1/directs", "directs") else {
        return;
    };
    let rooms = match super::rooms_of(saved) {
        super::Listed::Rows(rows) => rows,
        super::Listed::Empty => Vec::new(),
        super::Listed::Failed(detail) => {
            tracing::warn!("chatd: Tiny server: rooms: {detail}");
            return;
        }
    };
    let mut people: Vec<PersonRow> = names
        .iter()
        .filter(|(id, _)| super::safe_id(id))
        .map(|(id, name)| PersonRow { id: id.clone(), name: name.clone(), ..PersonRow::default() })
        .collect();
    let mut groups = Vec::new();
    let mut rows = Vec::new();
    let mut noted = Vec::new();
    let mut categories = HashSet::new();
    for channel in &channels {
        if !super::safe_id(&channel.id) {
            continue;
        }
        let mut row = ConversationRow::new(&channel.id, ConversationKind::Channel, title_or(&channel.name, &channel.id));
        row.space = Some(SPACE.to_owned());
        row.topic = channel.topic.clone();
        row.position = channel.position;
        row.encryption = encryption_of(saved, &channel.id, channel.e2ee);
        if let Some(category) = channel.category_id.as_deref().filter(|id| super::safe_id(id)) {
            row.group = Some(category.to_owned());
            if categories.insert(category.to_owned()) {
                let position = channels
                    .iter()
                    .filter(|other| other.category_id.as_deref() == Some(category))
                    .map(|other| other.position)
                    .min()
                    .unwrap_or(0);
                let name = channel.category_name.as_deref().filter(|name| !name.is_empty()).unwrap_or(category);
                groups.push(Group::new(category, name, position));
            }
        }
        noted.push(Note::from_last(&channel.id, channel.unread, None, None, channel.last_message.as_ref(), false).sealed(channel.e2ee));
        rows.push(row);
    }
    if !directs.is_empty() {
        let mut group = Group::new("directs", "Direct messages", DIRECTS_AT);
        group.kind = GroupKind::Directs;
        groups.push(group);
    }
    for (index, direct) in directs.iter().enumerate() {
        if !super::safe_id(&direct.id) || !super::safe_id(&direct.peer.user_id) {
            continue;
        }
        let name = title_or(&direct.peer.display_name, "Someone");
        if !people.iter().any(|person| person.id == direct.peer.user_id) {
            people.push(PersonRow { id: direct.peer.user_id.clone(), name: name.clone(), ..PersonRow::default() });
        }
        let mut row = ConversationRow::new(&direct.id, ConversationKind::Direct, name);
        row.space = Some(SPACE.to_owned());
        row.group = Some("directs".to_owned());
        row.peer = Some(direct.peer.user_id.clone());
        row.position = DIRECTS_AT + index as i32;
        row.encryption = encryption_of(saved, &direct.id, direct.e2ee);
        let mine = direct.last_message.as_ref().is_some_and(|last| last.author_id == saved.user_id);
        noted.push(Note::from_last(
            &direct.id,
            direct.unread,
            direct.last_read.as_deref(),
            direct.peer_read.as_deref(),
            direct.last_message.as_ref(),
            mine,
        ).sealed(direct.e2ee));
        rows.push(row);
    }
    if !rooms.is_empty() {
        groups.push(Group::new("rooms", "Rooms", ROOMS_AT));
    }
    for (index, room) in rooms.iter().enumerate() {
        if !super::safe_id(&room.id) {
            continue;
        }
        let mut row = ConversationRow::new(&room.id, ConversationKind::Voice, title_or(&room.name, &room.id));
        row.space = Some(SPACE.to_owned());
        row.group = Some("rooms".to_owned());
        row.position = ROOMS_AT + index as i32;
        row.call = CallSupport::Native;
        rows.push(row);
    }
    if !people.is_empty() {
        let _ = ctx.people(&people);
    }
    if ctx.tree(SPACE, &groups, &rows).is_err() {
        return;
    }
    for note in &noted {
        note.apply(ctx, saved, names);
    }
}

/// A durable frame, or one of the message frames a server sent without a seq.
pub(crate) fn on_live(ctx: &Context, saved: &Saved, names: &HashMap<String, String>, kind: &str, frame: &Value) {
    match kind {
        "channel.created" | "channel.updated" => channel_upsert(ctx, saved, names, frame),
        "channel.deleted" => {
            if let Some(id) = nested_id(frame, "channel", "channel_id") {
                let _ = ctx.conversation_gone(id);
            }
        }
        "direct.created" => direct_upsert(ctx, saved, names, frame),
        "message.created" | "message.edited" => {
            if let Some((conversation, wire)) = framed_message(frame) {
                deliver(ctx, saved, names, &conversation, &wire);
            }
        }
        "message.deleted" => {
            let conversation = nested_str(frame, "conversation_id");
            let message = nested_id(frame, "message", "message_id").unwrap_or("");
            if super::safe_id(conversation) && super::safe_id(message) {
                let _ = ctx.deleted(conversation, message);
            }
        }
        "reaction.updated" => merge_reaction(ctx, saved, frame),
        "read.updated" => read_updated(ctx, saved, frame),
        "server.updated" => ctx.capabilities(account_caps(saved)),
        // The group of an encrypted conversation changed, this device's key packages ran low, or
        // someone's devices changed. Off the socket's thread: it asks the server.
        "e2ee.handshake" | "e2ee.keys_low" | "e2ee.device" => {
            if let Some(device) = e2ee::started(saved) {
                let (kind, frame) = (kind.to_owned(), frame.clone());
                let _ = std::thread::Builder::new().name("tiny-e2ee".into()).spawn(move || {
                    device.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).heard(&kind, &frame);
                });
            }
        }
        _ => {}
    }
}

pub(crate) fn live_kind(kind: &str) -> bool {
    matches!(
        kind,
        "channel.created"
            | "channel.updated"
            | "channel.deleted"
            | "direct.created"
            | "message.created"
            | "message.edited"
            | "message.deleted"
            | "reaction.updated"
            | "read.updated"
            | "e2ee.handshake"
            | "e2ee.keys_low"
            | "e2ee.device"
    )
}

/// Someone is typing. Only a direct message shows it.
pub(crate) fn heard_typing(ctx: &Context, name: Option<&str>, frame: &Value) {
    let conversation_id = field(frame, "conversation_id");
    let user_id = field(frame, "user_id");
    if conversation_id.is_empty() || user_id.is_empty() || user_id == ctx.account.account {
        return;
    }
    let Ok(Some(row)) = ctx.known(conversation_id) else {
        return;
    };
    if row.kind != ConversationKind::Direct {
        return;
    }
    let named = field(frame, "display_name");
    let name = name.filter(|name| !name.is_empty()).unwrap_or(if named.is_empty() { "Someone" } else { named });
    ctx.typing(conversation_id, user_id, name, TYPING_FOR);
}

pub(crate) fn serve(ctx: &Context, saved: &Saved, live: &Live, command: Command) {
    match command {
        Command::Backfill(id) | Command::Warm(id) => load_page(ctx, saved, &id, None, PAGE),
        Command::History { conversation_id, before } => load_page(ctx, saved, &conversation_id, Some(before), PAGE),
        Command::HistoryAfter { conversation_id, .. } => load_page(ctx, saved, &conversation_id, None, PAGE),
        Command::HistoryAround { conversation_id, around } => {
            load_page(ctx, saved, &conversation_id, Some(around), PAGE);
            load_page(ctx, saved, &conversation_id, None, PAGE);
        }
        Command::Previews(ids) => {
            for id in ids.into_iter().take(protocol::ROWS_WATCHED) {
                load_page(ctx, saved, &id, None, 1);
            }
        }
        Command::OpenSpace(_) | Command::OpenHome => refresh(ctx, saved),
        Command::MarkRead { conversation_id, message_id } => mark_read(saved, &conversation_id, &message_id),
        Command::Send { conversation_id, client_id, text, reply_to } => {
            let outcome = post_message(ctx, saved, &conversation_id, &client_id, &text, reply_to.as_deref(), &[]);
            ctx.sent(&conversation_id, &client_id, outcome);
        }
        Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
            send_with(ctx, saved, conversation_id, client_id, text, reply_to, *with);
        }
        Command::CancelSend { client_id, .. } => upload::cancel(&ctx.account, &client_id),
        Command::React { conversation_id: _, message_id, emoji, on, reply } => react(saved, &message_id, &emoji, on, reply),
        Command::ReactionRules { reply, .. } => {
            let _ = reply.send(Ok(ReactionRules::default()));
        }
        Command::Edit { conversation_id, message_id, text, reply } => edit(ctx, saved, &conversation_id, &message_id, &text, reply),
        Command::Delete { conversation_id, message_id, reply } => delete(ctx, saved, &conversation_id, &message_id, reply),
        Command::Typing(conversation_id) => {
            if super::safe_id(&conversation_id) {
                live.ask(Ask::Type { conversation_id });
            }
        }
        Command::StartDirect { person_id, reply } => start_direct(ctx, saved, &person_id, reply),
        Command::Catalog { kind, reply, .. } => match kind {
            CatalogKind::Emoji | CatalogKind::Gifs => {
                let _ = reply.send(Ok(CatalogPage::default()));
            }
            _ => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
        },
        Command::Download { id, url, reply } => {
            if same_server(&saved.base_url, &url) {
                // A sealed attachment opens with what its message said; the cache opens it.
                let sealed = e2ee::started(saved).and_then(|device| device.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).sealed_file(&id));
                let _ = reply.send(Ok(Download {
                    url,
                    headers: vec![("Authorization".to_owned(), format!("Bearer {}", saved.token))],
                    sealed,
                }));
            } else {
                let _ = reply.send(Err(Refusal::Refused("That file is on another server.".to_owned())));
            }
        }
        other => other.unsupported(ctx),
    }
}

struct Note {
    id: String,
    /// The conversation is end-to-end encrypted: its last line has no body for the server.
    sealed: bool,
    unread: u32,
    last_read: Option<String>,
    peer_read: Option<String>,
    last: Option<WireLast>,
    /// The last line is one of ours, so a later `peer_read` can show Seen.
    mine: bool,
}

impl Note {
    fn from_last(id: &str, unread: u32, last_read: Option<&str>, peer_read: Option<&str>, last: Option<&WireLast>, mine: bool) -> Note {
        let kept = |value: Option<&str>| value.filter(|id| !id.is_empty() && super::safe_id(id)).map(str::to_owned);
        Note {
            id: id.to_owned(),
            sealed: false,
            unread,
            last_read: kept(last_read),
            peer_read: kept(peer_read),
            last: last.cloned(),
            mine,
        }
    }

    fn sealed(mut self, sealed: bool) -> Note {
        self.sealed = sealed;
        self
    }

    fn apply(&self, ctx: &Context, saved: &Saved, names: &HashMap<String, String>) {
        let updated = self.last.as_ref().and_then(|last| rfc3339(&last.created_at)).unwrap_or(if self.mine || self.peer_read.is_some() { 1 } else { 0 });
        // An empty direct message still belongs in the Inbox.
        let updated = if updated == 0 && self.peer_read.is_some() { 1 } else { updated };
        let direct = self.peer_read.is_some() || self.last_read.is_some();
        let updated = if updated == 0 && direct { 1 } else { updated };
        let _ = ctx.counts(&self.id, self.unread, 0, self.last_read.as_deref(), updated);
        if let Some(last) = &self.last {
            if super::safe_id(&last.id) {
                let text = if self.sealed { sealed_preview(ctx, &self.id, &last.id) } else { last.body.clone() };
                let _ = ctx.preview(&self.id, &PreviewLine {
                    id: last.id.clone(),
                    text,
                    sender: if self.mine { String::new() } else { name_of(names, saved, &last.author_id) },
                    mine: self.mine,
                    sent: rfc3339(&last.created_at).unwrap_or(updated),
                });
            }
        }
        if let Some(peer_read) = &self.peer_read {
            let _ = ctx.seen(&self.id, peer_read);
        }
    }
}

fn channel_upsert(ctx: &Context, saved: &Saved, names: &HashMap<String, String>, frame: &Value) {
    let Some(channel) = nested_value(frame, "channel").and_then(|value| serde_json::from_value::<WireChannel>(value).ok()) else {
        return;
    };
    if !super::safe_id(&channel.id) {
        return;
    }
    if let Some(category) = channel.category_id.as_deref().filter(|id| super::safe_id(id)) {
        let name = channel.category_name.as_deref().filter(|name| !name.is_empty()).unwrap_or(category);
        let _ = ctx.group(SPACE, &Group::new(category, name, channel.position));
    }
    let mut row = ConversationRow::new(&channel.id, ConversationKind::Channel, title_or(&channel.name, &channel.id));
    row.space = Some(SPACE.to_owned());
    row.group = channel.category_id.filter(|id| super::safe_id(id));
    row.topic = channel.topic.clone();
    row.position = channel.position;
    row.encryption = encryption_of(saved, &channel.id, channel.e2ee);
    if ctx.conversation(&row).is_err() {
        return;
    }
    Note::from_last(&channel.id, channel.unread, None, None, channel.last_message.as_ref(), false).sealed(channel.e2ee).apply(ctx, saved, names);
}

fn direct_upsert(ctx: &Context, saved: &Saved, names: &HashMap<String, String>, frame: &Value) {
    let Some(direct) = nested_value(frame, "direct").and_then(|value| serde_json::from_value::<WireDirect>(value).ok()) else {
        return;
    };
    if !super::safe_id(&direct.id) || !super::safe_id(&direct.peer.user_id) {
        return;
    }
    let name = title_or(&direct.peer.display_name, "Someone");
    let _ = ctx.people(&[PersonRow { id: direct.peer.user_id.clone(), name: name.clone(), ..PersonRow::default() }]);
    let mut group = Group::new("directs", "Direct messages", DIRECTS_AT);
    group.kind = GroupKind::Directs;
    let _ = ctx.group(SPACE, &group);
    let mut row = ConversationRow::new(&direct.id, ConversationKind::Direct, name);
    row.space = Some(SPACE.to_owned());
    row.group = Some("directs".to_owned());
    row.peer = Some(direct.peer.user_id.clone());
    row.position = DIRECTS_AT;
    row.encryption = encryption_of(saved, &direct.id, direct.e2ee);
    if ctx.conversation(&row).is_err() {
        return;
    }
    let mine = direct.last_message.as_ref().is_some_and(|last| last.author_id == saved.user_id);
    Note::from_last(&direct.id, direct.unread, direct.last_read.as_deref(), direct.peer_read.as_deref(), direct.last_message.as_ref(), mine).sealed(direct.e2ee).apply(ctx, saved, names);
}

fn deliver(ctx: &Context, saved: &Saved, names: &HashMap<String, String>, conversation_id: &str, wire: &WireMessage) {
    if !super::safe_id(conversation_id) {
        return;
    }
    let mut media = Vec::new();
    let opened = open_rows(saved, conversation_id, &[wire]);
    if wire.kind.as_deref() == Some("edit") {
        // An encrypted edit is a row of its own: what it changed is its target.
        refold(ctx, saved, conversation_id, &opened);
        return;
    }
    let Some(message) = map_message(saved, names, wire, &opened, &mut media) else {
        return;
    };
    if !media.is_empty() {
        let _ = ctx.media(&media);
    }
    let _ = ctx.message(conversation_id, &message);
}

/// An encrypted edit came: each target it folded into is shown again with its new text.
fn refold(ctx: &Context, saved: &Saved, conversation_id: &str, opened: &HashMap<String, Read>) {
    for (id, read) in opened {
        let Read::Opened(opened) = read else {
            continue;
        };
        let Ok(Some(mut message)) = ctx.stored_message(conversation_id, id) else {
            continue;
        };
        message.text = opened.text.clone();
        message.edited = true;
        let mut media = Vec::new();
        message.attachments = sealed_attachments(saved, opened, &mut media);
        if !media.is_empty() {
            let _ = ctx.media(&media);
        }
        let _ = ctx.message(conversation_id, &message);
    }
}

/// What the end-to-end encrypted rows of a page say. Empty for a plaintext page, and when this
/// device has no encryption state (its rows then read "Can't be decrypted on this device").
fn open_rows(saved: &Saved, conversation_id: &str, wires: &[&WireMessage]) -> HashMap<String, Read> {
    if !wires.iter().any(|wire| wire.e2ee) {
        return HashMap::new();
    }
    let Some(device) = e2ee::started(saved) else {
        return HashMap::new();
    };
    let rows: Vec<e2ee::Row> = wires
        .iter()
        .filter(|wire| wire.e2ee && !present(&wire.deleted_at))
        .map(|wire| e2ee::Row {
            id: &wire.id,
            author_id: &wire.author_id,
            kind: wire.kind.as_deref().unwrap_or("message"),
            ciphertext: wire.ciphertext.as_deref(),
            epoch: wire.epoch.unwrap_or(0),
            device_id: wire.device_id.as_deref().unwrap_or(""),
            target_id: wire.target_id.as_deref(),
            client_id: wire.client_id.as_deref(),
        })
        .collect();
    let mut device = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    device.note_encrypted(conversation_id, true);
    device.read(conversation_id, &rows)
}

/// The conversation's label: encrypted where the server says it is, and otherwise never.
fn encryption_of(saved: &Saved, conversation_id: &str, e2ee: bool) -> Encryption {
    if let Some(device) = e2ee::started(saved) {
        device.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).note_encrypted(conversation_id, e2ee);
    }
    if e2ee {
        Encryption::EndToEnd
    } else {
        Encryption::Plain
    }
}

/// The last line of an encrypted conversation, from what this device opened, or a word that says
/// there is one.
fn sealed_preview(ctx: &Context, conversation_id: &str, message_id: &str) -> String {
    match ctx.stored_message(conversation_id, message_id) {
        Ok(Some(message)) if message.seal == Seal::Encrypted => message.text,
        _ => "Encrypted message".to_owned(),
    }
}

fn merge_reaction(ctx: &Context, saved: &Saved, frame: &Value) {
    let conversation_id = field(frame, "conversation_id");
    let message_id = field(frame, "message_id");
    let emoji = field(frame, "emoji");
    if !super::safe_id(conversation_id) || !super::safe_id(message_id) || !unicode_emoji(emoji) {
        return;
    }
    let Ok(Some(mut message)) = ctx.stored_message(conversation_id, message_id) else {
        return;
    };
    let on = frame.get("on").and_then(Value::as_bool).unwrap_or(true);
    let count = match frame.get("count") {
        Some(value) => as_u32(value),
        None if !on => 0,
        None => 1,
    };
    let mine = (field(frame, "user_id") == saved.user_id).then_some(on);
    let at = message.reactions.iter().position(|have| have.emoji == emoji);
    if count == 0 {
        if let Some(at) = at {
            message.reactions.remove(at);
        }
    } else if let Some(at) = at {
        message.reactions[at].count = count;
        if let Some(mine) = mine {
            message.reactions[at].mine = mine;
        }
    } else {
        message.reactions.push(Reaction { emoji: emoji.to_owned(), file: None, count, mine: mine.unwrap_or(false) });
    }
    let _ = ctx.message(conversation_id, &message);
}

fn read_updated(ctx: &Context, saved: &Saved, frame: &Value) {
    let conversation_id = field(frame, "conversation_id");
    let message_id = field(frame, "message_id");
    let user_id = field(frame, "user_id");
    if !super::safe_id(conversation_id) || !super::safe_id(message_id) || user_id.is_empty() {
        return;
    }
    if user_id == saved.user_id {
        let _ = ctx.counts(conversation_id, 0, 0, Some(message_id), 0);
        return;
    }
    let Ok(Some(row)) = ctx.known(conversation_id) else {
        return;
    };
    if row.kind == ConversationKind::Direct && row.peer.as_deref() == Some(user_id) {
        let _ = ctx.seen(conversation_id, message_id);
    }
}

fn load_page(ctx: &Context, saved: &Saved, conversation_id: &str, before: Option<String>, limit: u32) {
    if !super::safe_id(conversation_id) {
        return;
    }
    let limit = limit.clamp(1, PAGE_MAX);
    let before = before.as_deref().filter(|id| super::safe_id(id));
    if let Err(detail) = fetch_page(ctx, saved, conversation_id, before, limit) {
        tracing::warn!("chatd: Tiny server: {conversation_id}: {detail}");
    }
}

fn fetch_page(ctx: &Context, saved: &Saved, conversation_id: &str, before: Option<&str>, limit: u32) -> Result<(), String> {
    let path = match before {
        Some(before) => format!("/v1/conversations/{conversation_id}/messages?before={before}&limit={limit}"),
        None => format!("/v1/conversations/{conversation_id}/messages?limit={limit}"),
    };
    match super::asked("GET", saved, &path, None) {
        Ok((200, body)) => {
            let wires: Vec<WireMessage> = list_of(body, "messages").map_err(|()| super::UNREADABLE.to_owned())?;
            let names = member_names(saved);
            let mut media = Vec::new();
            let opened = open_rows(saved, conversation_id, &wires.iter().collect::<Vec<_>>());
            let messages: Vec<Message> = wires.iter().filter(|wire| wire.kind.as_deref() != Some("edit")).filter_map(|wire| map_message(saved, &names, wire, &opened, &mut media)).collect();
            if !media.is_empty() {
                let _ = ctx.media(&media);
            }
            if !messages.is_empty() {
                ctx.history(conversation_id, &messages).map_err(|err| err.to_string())?;
            }
            Ok(())
        }
        Ok((403 | 404, _)) => Ok(()),
        Ok((status, body)) => Err(super::explain(status, &body, super::Missing::Plain).to_owned()),
        Err(detail) => Err(detail.to_owned()),
    }
}

fn post_message(ctx: &Context, saved: &Saved, conversation_id: &str, client_id: &str, text: &str, reply_to: Option<&str>, attachment_ids: &[String]) -> Result<Message, String> {
    if !super::safe_id(conversation_id) {
        return Err(super::CHECK.to_owned());
    }
    if sealed_here(ctx, saved, conversation_id)? {
        return post_sealed(ctx, saved, conversation_id, client_id, text, reply_to, Vec::new(), attachment_ids);
    }
    let mut body = serde_json::Map::new();
    body.insert("body".to_owned(), Value::String(text.to_owned()));
    body.insert("client_id".to_owned(), Value::String(client_id.to_owned()));
    if let Some(reply_to) = reply_to.filter(|id| super::safe_id(id)) {
        body.insert("reply_to".to_owned(), Value::String(reply_to.to_owned()));
    }
    if !attachment_ids.is_empty() {
        body.insert("attachment_ids".to_owned(), json!(attachment_ids));
    }
    let path = format!("/v1/conversations/{conversation_id}/messages");
    match super::asked("POST", saved, &path, Some(Value::Object(body))) {
        Ok((200 | 201, value)) => accept_message(ctx, saved, conversation_id, &value),
        Ok((status, value)) => Err(super::explain(status, &value, super::Missing::Plain).to_owned()),
        Err(detail) => Err(detail.to_owned()),
    }
}

/// Words for an encrypted conversation this device cannot send to yet.
const NOT_SET_UP: &str = "This device isn't set up for encrypted messages yet. Open Encryption in the server's settings.";

/// Whether a send to this conversation must be end-to-end encrypted. An encrypted conversation is
/// never sent plaintext, whatever state this device is in: it is an error instead.
fn sealed_here(ctx: &Context, saved: &Saved, conversation_id: &str) -> Result<bool, String> {
    let labelled = ctx.known(conversation_id).ok().flatten().is_some_and(|row| row.encryption == Encryption::EndToEnd);
    let Some(device) = e2ee::started(saved) else {
        return if labelled { Err(NOT_SET_UP.to_owned()) } else { Ok(false) };
    };
    let mut device = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let encrypted = labelled || device.is_encrypted(conversation_id);
    if encrypted && !device.ready() {
        return Err(NOT_SET_UP.to_owned());
    }
    Ok(encrypted)
}

/// An end-to-end encrypted message: the envelope is encrypted by this device and posted as
/// ciphertext. What the server answers is read back through the cache, which holds the text.
#[allow(clippy::too_many_arguments)]
fn post_sealed(ctx: &Context, saved: &Saved, conversation_id: &str, client_id: &str, text: &str, reply_to: Option<&str>, attachments: Vec<e2ee_core::AttachmentRef>, attachment_ids: &[String]) -> Result<Message, String> {
    let device = e2ee::started(saved).ok_or(NOT_SET_UP)?;
    let mut envelope = e2ee_core::Envelope::message(text);
    envelope.reply_to = reply_to.filter(|id| super::safe_id(id)).map(str::to_owned);
    envelope.attachments = attachments;
    envelope.sent_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|since| since.as_millis() as i64);
    let answer = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).post(conversation_id, client_id, e2ee_core::Kind::Message, None, &envelope, attachment_ids)?;
    accept_message(ctx, saved, conversation_id, &answer)
}

fn send_with(ctx: &Context, saved: &Saved, conversation_id: String, client_id: String, text: String, reply_to: Option<String>, outgoing: Outgoing) {
    if outgoing.files.is_empty() {
        let outcome = post_message(ctx, saved, &conversation_id, &client_id, &text, reply_to.as_deref(), &[]);
        ctx.sent(&conversation_id, &client_id, outcome);
        return;
    }
    if outgoing.files.len() > protocol::ATTACHMENTS_LIMIT {
        ctx.sent(&conversation_id, &client_id, Err("Too many files.".to_owned()));
        return;
    }
    let limits = upload_limits(saved);
    for file in &outgoing.files {
        let cap = cap_for(file, &limits);
        let len = std::fs::metadata(&file.path).map(|meta| meta.len()).unwrap_or(0);
        if len > cap {
            ctx.sent(&conversation_id, &client_id, Err("That file is too big for this server.".to_owned()));
            return;
        }
    }
    let sealed = match sealed_here(ctx, saved, &conversation_id) {
        Ok(sealed) => sealed,
        Err(why) => {
            ctx.sent(&conversation_id, &client_id, Err(why));
            return;
        }
    };
    let saved = saved.clone();
    let posting = ctx.clone();
    let files = outgoing.files;
    let posting_id = conversation_id.clone();
    let posting_client = client_id.clone();
    if sealed {
        upload::spawn(ctx, &conversation_id, &client_id, &files, move |upload| {
            let refs = super::sealing::upload_sealed(upload, &saved, &posting_id)?;
            upload.check()?;
            let ids: Vec<String> = refs.iter().map(|file| file.id.clone()).collect();
            post_sealed(&posting, &saved, &posting_id, &posting_client, &text, reply_to.as_deref(), refs, &ids)
        });
        return;
    }
    upload::spawn(ctx, &conversation_id, &client_id, &files, move |upload| {
        let mut ids = Vec::with_capacity(upload.files().len());
        for at in 0..upload.files().len() {
            upload.check()?;
            let target = Target::post(super::endpoint(&saved.base_url, "/v1/media")).header("Authorization", &format!("Bearer {}", saved.token));
            let answer = match upload.send_form(at, &target, "file", &[]) {
                Ok(answer) => answer,
                Err(why) => return Err(media_failure(&why)),
            };
            let body: Value = serde_json::from_str(&answer.body).unwrap_or(Value::Null);
            let id = body.get("id").and_then(Value::as_str).or_else(|| body.pointer("/media/id").and_then(Value::as_str)).unwrap_or("");
            if !super::safe_id(id) {
                return Err(super::UNREADABLE.to_owned());
            }
            ids.push(id.to_owned());
        }
        upload.check()?;
        post_message(&posting, &saved, &posting_id, &posting_client, &text, reply_to.as_deref(), &ids)
    });
}

fn react(saved: &Saved, message_id: &str, emoji: &str, on: bool, reply: crate::adapters::Reply<()>) {
    if !unicode_emoji(emoji) {
        let _ = reply.send(Err(Refusal::Refused("That reaction isn't allowed here.".to_owned())));
        return;
    }
    if !super::safe_id(message_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    let path = format!("/v1/messages/{message_id}/reactions");
    let method = if on { "PUT" } else { "DELETE" };
    let answer = super::asked(method, saved, &path, Some(json!({ "emoji": emoji })));
    answer_ok(answer, reply);
}

fn edit(ctx: &Context, saved: &Saved, conversation_id: &str, message_id: &str, text: &str, reply: crate::adapters::Reply<()>) {
    if !super::safe_id(message_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    match sealed_here(ctx, saved, conversation_id) {
        Ok(true) => {
            let _ = reply.send(edit_sealed(ctx, saved, conversation_id, message_id, text).map_err(Refusal::Refused));
            return;
        }
        Ok(false) => {}
        Err(why) => {
            let _ = reply.send(Err(Refusal::Refused(why)));
            return;
        }
    }
    let path = format!("/v1/messages/{message_id}");
    match super::asked("PATCH", saved, &path, Some(json!({ "body": text }))) {
        Ok((200 | 201, value)) => {
            if let Ok(message) = accept_message(ctx, saved, conversation_id, &value) {
                let _ = ctx.message(conversation_id, &message);
            } else if let Ok(Some(mut message)) = ctx.stored_message(conversation_id, message_id) {
                message.text = text.to_owned();
                message.edited = true;
                let _ = ctx.message(conversation_id, &message);
            }
            let _ = reply.send(Ok(()));
        }
        other => answer_ok(other, reply),
    }
}

/// An edit of an encrypted message: a ciphertext of kind `edit` that names it. It keeps the
/// message's attachments; every member's device folds it in.
fn edit_sealed(ctx: &Context, saved: &Saved, conversation_id: &str, message_id: &str, text: &str) -> Result<(), String> {
    let device = e2ee::started(saved).ok_or(NOT_SET_UP)?;
    let mut device = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut envelope = e2ee_core::Envelope::message(text);
    envelope.attachments = device.attachments_of(conversation_id, message_id);
    let client_id = format!("edit-{message_id}-{}", crate::hub::now());
    device.post(conversation_id, &client_id, e2ee_core::Kind::Edit, Some(message_id), &envelope, &[])?;
    drop(device);
    if let Ok(Some(mut message)) = ctx.stored_message(conversation_id, message_id) {
        message.text = text.to_owned();
        message.edited = true;
        let _ = ctx.message(conversation_id, &message);
    }
    Ok(())
}

fn delete(ctx: &Context, saved: &Saved, conversation_id: &str, message_id: &str, reply: crate::adapters::Reply<()>) {
    if !super::safe_id(message_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    let path = format!("/v1/messages/{message_id}");
    match super::asked("DELETE", saved, &path, None) {
        Ok((200 | 204, _)) => {
            let _ = ctx.deleted(conversation_id, message_id);
            let _ = reply.send(Ok(()));
        }
        other => answer_ok(other, reply),
    }
}

fn mark_read(saved: &Saved, conversation_id: &str, message_id: &str) {
    if !super::safe_id(conversation_id) || !super::safe_id(message_id) {
        return;
    }
    let path = format!("/v1/conversations/{conversation_id}/read");
    match super::asked("POST", saved, &path, Some(json!({ "message_id": message_id }))) {
        Ok((200 | 204, _)) => {}
        Ok((status, _)) => tracing::warn!("chatd: Tiny server: read answered {status}"),
        Err(detail) => tracing::warn!("chatd: Tiny server: read: {detail}"),
    }
}

fn start_direct(ctx: &Context, saved: &Saved, person_id: &str, reply: crate::adapters::Reply<String>) {
    if !super::safe_id(person_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    // A new direct message is end-to-end encrypted when this device can be in one and the server
    // allows it; a server that requires it gets nothing else.
    let mode = e2ee::started(saved).map(|device| {
        let device = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (device.ready(), device.mode().to_owned())
    });
    let encrypted = match mode {
        Some((true, mode)) => mode != "off" && !mode.is_empty(),
        Some((false, mode)) if mode == "new_required" => {
            let _ = reply.send(Err(Refusal::Refused(NOT_SET_UP.to_owned())));
            return;
        }
        _ => false,
    };
    match super::asked("POST", saved, "/v1/directs", Some(json!({ "user_id": person_id, "e2ee": encrypted }))) {
        Ok((200 | 201, body)) => {
            let id = body.get("id").and_then(Value::as_str).or_else(|| body.pointer("/direct/id").and_then(Value::as_str)).unwrap_or("");
            if !super::safe_id(id) {
                let _ = reply.send(Err(Refusal::Refused(super::UNREADABLE.to_owned())));
                return;
            }
            refresh(ctx, saved);
            if ctx.known(id).ok().flatten().is_none() {
                let mut row = ConversationRow::new(id, ConversationKind::Direct, "Someone");
                row.space = Some(SPACE.to_owned());
                row.group = Some("directs".to_owned());
                row.peer = Some(person_id.to_owned());
                row.position = DIRECTS_AT;
                let _ = ctx.conversation(&row);
                let _ = ctx.counts(id, 0, 0, None, 1);
            }
            let _ = reply.send(Ok(id.to_owned()));
        }
        Ok((status, body)) => {
            let _ = reply.send(Err(Refusal::Refused(super::explain(status, &body, super::Missing::Plain).to_owned())));
        }
        Err(detail) => {
            let _ = reply.send(Err(Refusal::Refused(detail.to_owned())));
        }
    }
}

fn answer_ok(answer: Result<(u16, Value), &'static str>, reply: crate::adapters::Reply<()>) {
    match answer {
        Ok((200 | 201 | 204, _)) => {
            let _ = reply.send(Ok(()));
        }
        Ok((status, body)) => {
            let _ = reply.send(Err(Refusal::Refused(super::explain(status, &body, super::Missing::Plain).to_owned())));
        }
        Err(detail) => {
            let _ = reply.send(Err(Refusal::Refused(detail.to_owned())));
        }
    }
}

fn accept_message(ctx: &Context, saved: &Saved, conversation: &str, value: &Value) -> Result<Message, String> {
    let wire = one_wire(value).ok_or(super::UNREADABLE)?;
    let mut media = Vec::new();
    let opened = open_rows(saved, conversation, &[&wire]);
    let message = map_message(saved, &member_names(saved), &wire, &opened, &mut media).ok_or(super::UNREADABLE)?;
    if !media.is_empty() {
        let _ = ctx.media(&media);
    }
    Ok(message)
}

fn map_message(saved: &Saved, names: &HashMap<String, String>, wire: &WireMessage, opened: &HashMap<String, Read>, media: &mut Vec<(MediaFile, String)>) -> Option<Message> {
    if !super::safe_id(&wire.id) {
        return None;
    }
    let deleted = present(&wire.deleted_at);
    let sent = rfc3339(&wire.created_at).unwrap_or(0);
    let outgoing = wire.author_id == saved.user_id;
    if wire.e2ee {
        return map_sealed(saved, names, wire, opened.get(&wire.id), deleted, sent, outgoing, media);
    }
    let mut message = Message::plain(&wire.id, name_of(names, saved, &wire.author_id), if deleted { "" } else { &wire.body }, sent, outgoing);
    message.sender.id = wire.author_id.clone();
    message.edited = present(&wire.edited_at);
    message.deleted = deleted;
    message.client_id = wire.client_id.clone().filter(|id| !id.is_empty());
    message.reply_to = reply_of(wire.reply_to.as_ref());
    if !deleted {
        for file in &wire.attachments {
            if !super::safe_id(&file.id) {
                continue;
            }
            let kind = media_kind(&file.kind, &file.mime);
            let mut stored = MediaFile::new(&file.id, kind);
            stored.width = file.width;
            stored.height = file.height;
            stored.bytes = file.bytes;
            let mut attachment = Attachment::new(title_or(&file.name, &file.id), stored);
            attachment.mime = file.mime.clone();
            attachment.duration = file.duration;
            media.push((attachment.file.clone(), fetched_url(saved, file)));
            message.attachments.push(attachment);
        }
    }
    for reaction in &wire.reactions {
        if reaction.count == 0 || !unicode_emoji(&reaction.emoji) {
            continue;
        }
        message.reactions.push(Reaction { emoji: reaction.emoji.clone(), file: None, count: reaction.count, mine: reaction.me });
    }
    Some(message)
}

/// An end-to-end encrypted row: what this device opened, or the words that say it could not.
/// The server's `body` is never read for one: it is empty, and a server that filled it is not
/// believed.
#[allow(clippy::too_many_arguments)]
fn map_sealed(saved: &Saved, names: &HashMap<String, String>, wire: &WireMessage, read: Option<&Read>, deleted: bool, sent: i64, outgoing: bool, media: &mut Vec<(MediaFile, String)>) -> Option<Message> {
    let sender = name_of(names, saved, &wire.author_id);
    let mut message = Message::plain(&wire.id, sender, "", sent, outgoing);
    message.sender.id = wire.author_id.clone();
    message.deleted = deleted;
    message.client_id = wire.client_id.clone().filter(|id| !id.is_empty());
    message.seal = Seal::Encrypted;
    if !deleted {
        match read {
            Some(Read::Opened(opened)) => {
                message.text = opened.text.clone();
                message.edited = opened.edited_by.is_some();
                message.reply_to = reply_of(opened.reply_to.as_ref().map(|id| Value::String(id.clone())).as_ref());
                message.attachments = sealed_attachments(saved, opened, media);
            }
            Some(Read::Edit) => return None,
            Some(Read::Undecryptable) | None => {
                message.text = protocol::UNDECRYPTABLE.to_owned();
                message.seal = Seal::Undecryptable;
            }
        }
    }
    for reaction in &wire.reactions {
        if reaction.count == 0 || !unicode_emoji(&reaction.emoji) {
            continue;
        }
        message.reactions.push(Reaction { emoji: reaction.emoji.clone(), file: None, count: reaction.count, mine: reaction.me });
    }
    Some(message)
}

/// The attachments an opened message names, registered at the server's address for each sealed
/// blob. The kind comes from what the sender says the file is; the cache checks the opened bytes.
fn sealed_attachments(saved: &Saved, opened: &e2ee::Opened, media: &mut Vec<(MediaFile, String)>) -> Vec<Attachment> {
    let mut out = Vec::new();
    for file in &opened.attachments {
        if !super::safe_id(&file.id) {
            continue;
        }
        let kind = match media_kind("", &file.mime) {
            // A picture is shown as one only in a format the cache checks; anything else is a file.
            MediaKind::Image if matches!(file.mime.as_str(), "image/png" | "image/jpeg" | "image/webp") => MediaKind::Image,
            MediaKind::Gif => MediaKind::Gif,
            _ => MediaKind::File,
        };
        let mut stored = MediaFile::new(&file.id, kind);
        stored.width = file.width;
        stored.height = file.height;
        stored.bytes = file.bytes;
        let mut attachment = Attachment::new(title_or(&file.name, "file"), stored);
        attachment.mime = file.mime.clone();
        media.push((attachment.file.clone(), format!("{}/v1/media/{}", saved.base_url.trim_end_matches('/'), file.id)));
        out.push(attachment);
    }
    out
}

fn media_kind(kind: &str, mime: &str) -> MediaKind {
    match kind {
        "image" => MediaKind::Image,
        "gif" => MediaKind::Gif,
        "video" => MediaKind::Video,
        "file" => MediaKind::File,
        _ => {
            let mime = mime.to_ascii_lowercase();
            if mime == "image/gif" {
                MediaKind::Gif
            } else if mime.starts_with("video/") {
                MediaKind::Video
            } else if mime.starts_with("image/") {
                MediaKind::Image
            } else {
                MediaKind::File
            }
        }
    }
}

/// Unicode emoji only. A shortcode, a word, or a path is not one.
fn unicode_emoji(emoji: &str) -> bool {
    let mut textual = false;
    if emoji.is_empty() {
        return false;
    }
    for ch in emoji.chars() {
        if ch.is_ascii_alphanumeric() || ch == ':' || ch == '/' {
            return false;
        }
        if !ch.is_ascii() {
            textual = true;
        }
    }
    textual
}

fn same_server(base: &str, url: &str) -> bool {
    let base = base.trim_end_matches('/');
    !base.is_empty() && (url == base || url.starts_with(&format!("{base}/")))
}

struct UploadLimits {
    image: u64,
    file: u64,
    archives: bool,
}

/// What this account may send. `file_scan` is absent on an older server, which keeps today's upload.
fn account_caps(saved: &Saved) -> Capabilities {
    let limits = upload_limits(saved);
    let scans = super::manage::capabilities_of(saved).has(protocol::TINY_FEATURE_FILE_SCAN);
    Capabilities {
        max_file_bytes: limits.image.max(limits.file),
        max_files: protocol::ATTACHMENTS_LIMIT as u32,
        file_scan: scans,
        allow_archives: scans && limits.archives,
        ..Capabilities::default()
    }
}

/// The caps `GET /v1/server` reports. An older server, or one that does not answer, keeps the original 8 MiB and 25 MiB.
fn upload_limits(saved: &Saved) -> UploadLimits {
    let fallback = UploadLimits { image: protocol::TINY_IMAGE_MAX_DEFAULT as u64, file: protocol::TINY_FILE_MAX_DEFAULT as u64, archives: false };
    let Ok((200, body)) = super::asked("GET", saved, "/v1/server", None) else {
        return fallback;
    };
    UploadLimits {
        image: byte_cap(&body, "image_max_bytes", fallback.image),
        file: byte_cap(&body, "file_max_bytes", fallback.file),
        archives: body.get("allow_archives").and_then(Value::as_bool).unwrap_or(false),
    }
}

/// The upload helper names a Tiny refusal `tiny-media`, then the status, then the body. The
/// server's `error` becomes the sentence the chip shows. Anything else is left as it arrived.
pub(crate) fn media_failure(why: &str) -> String {
    let Some(rest) = why.strip_prefix("tiny-media\n") else {
        return why.to_owned();
    };
    let Some((status, body)) = rest.split_once('\n') else {
        return why.to_owned();
    };
    let Ok(status) = status.parse::<u16>() else {
        return why.to_owned();
    };
    let body = serde_json::from_str(body).unwrap_or(Value::Null);
    super::explain_media(status, &body).to_owned()
}

/// The address the file is fetched from. An absolute `url` on this server is kept. A path
/// (`/v1/media/{id}?ex=&is=&hm=`, which is what mint returns) is joined onto the server base
/// so the query is not dropped. Anything else uses the plain media path. The bearer is added
/// when the file is fetched, not here.
fn fetched_url(saved: &Saved, file: &WireFile) -> String {
    let offered = file.url.trim();
    let base = saved.base_url.trim_end_matches('/');
    if !offered.is_empty() && same_server(base, offered) {
        return offered.to_owned();
    }
    if let Some(path) = offered.strip_prefix('/').filter(|path| !path.starts_with('/')) {
        return format!("{base}/{path}");
    }
    format!("{base}/v1/media/{}", file.id)
}

fn byte_cap(body: &Value, key: &str, fallback: u64) -> u64 {
    let Some(bytes) = body.get(key).and_then(Value::as_i64).and_then(|bytes| u64::try_from(bytes).ok()) else {
        return fallback;
    };
    let ceiling = protocol::TINY_UPLOAD_MAX as u64;
    if (1..=ceiling).contains(&bytes) { bytes } else { fallback }
}

fn cap_for(file: &protocol::OutgoingFile, limits: &UploadLimits) -> u64 {
    let named = if file.name.is_empty() { file.path.as_str() } else { file.name.as_str() };
    let ext = named.rsplit(['/', '\\', '.']).next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" => limits.image,
        _ => limits.file,
    }
}

fn member_names(saved: &Saved) -> HashMap<String, String> {
    super::members_of(saved).map(|members| members.into_iter().map(|member| (member.user_id, member.display_name)).collect()).unwrap_or_default()
}

fn name_of(names: &HashMap<String, String>, saved: &Saved, author: &str) -> String {
    if author == saved.user_id && !saved.display_name.is_empty() {
        return saved.display_name.clone();
    }
    names.get(author).filter(|name| !name.is_empty()).cloned().unwrap_or_else(|| "Someone".to_owned())
}

fn title_or(name: &str, fallback: &str) -> String {
    if name.is_empty() { fallback.to_owned() } else { name.to_owned() }
}

fn present(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|text| !text.is_empty())
}

fn optional_list<T: serde::de::DeserializeOwned>(saved: &Saved, path: &str, key: &str) -> Option<Vec<T>> {
    match super::asked("GET", saved, path, None) {
        Ok((403 | 404, _)) => Some(Vec::new()),
        Ok((200, body)) => match list_of(body, key) {
            Ok(rows) => Some(rows),
            Err(()) => {
                tracing::warn!("chatd: Tiny server: {path} could not be read");
                None
            }
        },
        Ok((status, _)) => {
            tracing::warn!("chatd: Tiny server: {path} answered {status}");
            None
        }
        Err(detail) => {
            tracing::warn!("chatd: Tiny server: {path}: {detail}");
            None
        }
    }
}

fn list_of<T: serde::de::DeserializeOwned>(body: Value, key: &str) -> Result<Vec<T>, ()> {
    let value = body.get(key).cloned().unwrap_or(body);
    serde_json::from_value(value).map_err(|_| ())
}

fn one_wire(value: &Value) -> Option<WireMessage> {
    let value = value.get("message").unwrap_or(value);
    serde_json::from_value(value.clone()).ok()
}

fn framed_message(frame: &Value) -> Option<(String, WireMessage)> {
    let conversation = {
        let id = field(frame, "conversation_id");
        if !id.is_empty() {
            id.to_owned()
        } else {
            frame.get("message").map(|message| field(message, "conversation_id").to_owned()).unwrap_or_default()
        }
    };
    let wire = one_wire(frame.get("message").unwrap_or(frame))?;
    if conversation.is_empty() { None } else { Some((conversation, wire)) }
}

fn nested_value(frame: &Value, key: &str) -> Option<Value> {
    Some(frame.get(key).unwrap_or(frame).clone())
}

fn nested_id<'a>(frame: &'a Value, object: &str, field_name: &str) -> Option<&'a str> {
    let id = field(frame, field_name);
    if !id.is_empty() {
        return Some(id);
    }
    let id = frame.get(object).map(|value| field(value, "id")).unwrap_or("");
    if id.is_empty() { None } else { Some(id) }
}

fn nested_str<'a>(frame: &'a Value, key: &str) -> &'a str {
    let id = field(frame, key);
    if !id.is_empty() {
        return id;
    }
    frame.get("message").map(|message| field(message, key)).unwrap_or("")
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn reply_of(value: Option<&Value>) -> Option<ReplyTo> {
    let id = match value? {
        Value::String(id) => id.clone(),
        Value::Object(map) => map.get("id").and_then(Value::as_str)?.to_owned(),
        _ => return None,
    };
    if id.is_empty() { None } else { Some(ReplyTo { id, sender: String::new(), snippet: String::new(), person: None, deleted: false }) }
}

fn as_u32(value: &Value) -> u32 {
    match value {
        Value::Bool(on) => u32::from(*on),
        Value::Number(number) => number.as_u64().or_else(|| number.as_f64().map(|number| number.max(0.0) as u64)).unwrap_or(0).min(u32::MAX as u64) as u32,
        Value::String(text) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn as_u64(value: &Value) -> u64 {
    match value {
        Value::Number(number) => number.as_u64().or_else(|| number.as_f64().map(|number| number.max(0.0) as u64)).unwrap_or(0),
        Value::String(text) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn flex_u32<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    Ok(as_u32(&Value::deserialize(deserializer)?))
}

fn flex_u64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    Ok(as_u64(&Value::deserialize(deserializer)?))
}

/// Unix seconds of a server timestamp.
pub(crate) fn unix_of(text: &str) -> Option<i64> {
    rfc3339(text)
}

/// `2024-01-02T03:04:05Z`, with a fraction and a numeric offset.
fn rfc3339(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.len() < 20 {
        return None;
    }
    let bytes = text.as_bytes();
    let tee = bytes.get(10).copied()?;
    if bytes[4] != b'-' || bytes[7] != b'-' || (tee != b'T' && tee != b't') || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year: i32 = text[0..4].parse().ok()?;
    let month: u32 = text[5..7].parse().ok()?;
    let day: u32 = text[8..10].parse().ok()?;
    let hour: u32 = text[11..13].parse().ok()?;
    let minute: u32 = text[14..16].parse().ok()?;
    let second: u32 = text[17..19].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &text[19..];
    if let Some(stripped) = rest.strip_prefix('.') {
        let digits = stripped.chars().take_while(|ch| ch.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        rest = &stripped[digits..];
    }
    let offset = if rest == "Z" || rest == "z" {
        0
    } else if rest.len() == 6 && (rest.starts_with('+') || rest.starts_with('-')) && rest.as_bytes()[3] == b':' {
        let sign = if rest.starts_with('+') { 1 } else { -1 };
        let hours: i64 = rest[1..3].parse().ok()?;
        let mins: i64 = rest[4..6].parse().ok()?;
        if hours > 23 || mins > 59 {
            return None;
        }
        sign * (hours * 3600 + mins * 60)
    } else {
        return None;
    };
    let days = days_from_civil(year, month, day)?;
    Some(days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64 - offset)
}

/// Days since 1970-01-01. Howard Hinnant's civil-from-days algorithm, inverted.
fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let yoe = (year - era * 400) as u64;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month_prime as u64 + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era as i64 * 146097 + doe as i64 - 719468)
}

#[derive(Clone, Deserialize)]
struct WireChannel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    topic: String,
    #[serde(default)]
    category_id: Option<String>,
    #[serde(default)]
    category_name: Option<String>,
    #[serde(default)]
    position: i32,
    #[serde(default, deserialize_with = "flex_u32")]
    unread: u32,
    #[serde(default)]
    last_message: Option<WireLast>,
    /// End-to-end encrypted, for good. Absent on a server older than encrypted text.
    #[serde(default)]
    e2ee: bool,
}

#[derive(Clone, Deserialize)]
struct WireDirect {
    id: String,
    peer: WirePeer,
    #[serde(default)]
    e2ee: bool,
    #[serde(default, deserialize_with = "flex_u32")]
    unread: u32,
    #[serde(default)]
    last_read: Option<String>,
    #[serde(default)]
    peer_read: Option<String>,
    #[serde(default)]
    last_message: Option<WireLast>,
}

#[derive(Clone, Deserialize)]
struct WirePeer {
    user_id: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Clone, Deserialize)]
struct WireLast {
    id: String,
    #[serde(default)]
    author_id: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
}

#[derive(Deserialize)]
struct WireMessage {
    id: String,
    #[serde(default)]
    author_id: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    reply_to: Option<Value>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    edited_at: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
    #[serde(default)]
    attachments: Vec<WireFile>,
    #[serde(default)]
    reactions: Vec<WireReaction>,
    /// An end-to-end encrypted row: `body` is empty, and these say what it is.
    #[serde(default)]
    e2ee: bool,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    ciphertext: Option<String>,
    #[serde(default)]
    epoch: Option<u64>,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    target_id: Option<String>,
}

#[derive(Deserialize)]
struct WireFile {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    mime: String,
    #[serde(default)]
    kind: String,
    #[serde(default, deserialize_with = "flex_u64")]
    bytes: u64,
    #[serde(default, deserialize_with = "flex_u32")]
    width: u32,
    #[serde(default, deserialize_with = "flex_u32")]
    height: u32,
    #[serde(default, deserialize_with = "flex_u32")]
    duration: u32,
    /// A signed fetch address (`ex`, `is`, `hm`). Empty on an older server: the id's path is used.
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct WireReaction {
    emoji: String,
    #[serde(default, deserialize_with = "flex_u32")]
    count: u32,
    #[serde(default)]
    me: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::local;
    use crate::hub::Hub;
    use crate::store::Store;
    use protocol::{AccountKey, InboxFilter, Network, Page, SendState};
    use std::sync::Arc;

    const ME: &str = "11111111-1111-1111-1111-111111111111";
    const PEER: &str = "22222222-2222-2222-2222-222222222222";
    const WHEN: &str = "2024-01-02T03:04:05Z";

    fn http(status: u16, reason: &str, body: &str) -> Vec<u8> {
        super::super::tests::http(status, reason, body)
    }

    fn saved(base: &str) -> Saved {
        Saved {
            base_url: base.to_owned(),
            token: "t".to_owned(),
            user_id: ME.to_owned(),
            email: Some("ada@example.com".to_owned()),
            display_name: "Ada".to_owned(),
            role: "owner".to_owned(),
            plain_http_ok: false,
            e2ee: None,
        }
    }

    fn open_ctx() -> (crate::adapters::Adapter, Context) {
        let hub = Arc::new(Hub::new(Store::open(std::path::Path::new(":memory:")).expect("store")));
        let account = AccountKey { network: Network::Tiny, account: ME.to_owned() };
        let (adapter, ctx, _inbox) = local(account, hub);
        (adapter, ctx)
    }

    fn lists(request: &str) -> Vec<u8> {
        if request.starts_with("GET /v1/channels ") {
            return http(200, "OK", &format!(
                r#"{{"channels":[{{"id":"channel-1","name":"general","topic":"Hello","category_id":"cat-1","category_name":"Text","position":1,"unread":2,"last_message":{{"id":"c-9","author_id":"{PEER}","body":"hey","created_at":"{WHEN}"}}}}]}}"#
            ));
        }
        if request.starts_with("GET /v1/directs ") {
            return http(200, "OK", &format!(
                r#"{{"directs":[{{"id":"direct-1","peer":{{"user_id":"{PEER}","display_name":"Bea"}},"unread":1,"last_read":"m-1","last_message":{{"id":"m-2","author_id":"{ME}","body":"mine","created_at":"{WHEN}"}}}}]}}"#
            ));
        }
        if request.starts_with("GET /v1/rooms ") {
            return http(200, "OK", r#"[{"id":"room-1","name":"Lounge"}]"#);
        }
        if request.starts_with("GET /v1/members ") {
            return http(200, "OK", &format!(
                r#"[{{"user_id":"{ME}","email":"ada@example.com","display_name":"Ada","role":"owner","created_at":"{WHEN}"}},{{"user_id":"{PEER}","email":"bea@example.com","display_name":"Bea","role":"member","created_at":"{WHEN}"}}]"#
            ));
        }
        http(404, "Not Found", "{}")
    }

    fn fixture(more: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static) -> (crate::adapters::Adapter, Context, Saved) {
        let base = super::super::tests::serve(move |request| more(request).unwrap_or_else(|| lists(request)));
        let saved = saved(&base);
        let (adapter, ctx) = open_ctx();
        refresh(&ctx, &saved);
        (adapter, ctx, saved)
    }

    #[test]
    fn a_timestamp_is_unix_seconds() {
        assert_eq!(rfc3339("2024-01-02T03:04:05Z"), Some(1_704_164_645));
        assert_eq!(rfc3339("2024-01-02T04:04:05+01:00"), Some(1_704_164_645));
        assert_eq!(rfc3339("2024-01-02T03:04:05.123Z"), Some(1_704_164_645));
        assert_eq!(rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert!(rfc3339("yesterday").is_none());
        assert!(unicode_emoji("👍"));
        assert!(unicode_emoji("👍\u{FE0F}"));
        assert!(!unicode_emoji(":smile:"));
        assert!(!unicode_emoji("ok"));
        assert!(!unicode_emoji(""));
    }

    #[test]
    fn channel_and_direct_rows_keep_voice_rooms() {
        let (_adapter, ctx, _saved) = fixture(|_| None);
        let channel = ctx.known("channel-1").unwrap().unwrap();
        assert_eq!(channel.kind, ConversationKind::Channel);
        assert_eq!(channel.title, "general");
        assert_eq!(channel.unread, 2);
        assert_eq!(channel.group_title, "Text");
        let direct = ctx.known("direct-1").unwrap().unwrap();
        assert_eq!(direct.kind, ConversationKind::Direct);
        assert_eq!(direct.title, "Bea");
        assert_eq!(direct.peer.as_deref(), Some(PEER));
        assert!(direct.preview_mine);
        assert!(!direct.seen);
        let room = ctx.known("room-1").unwrap().unwrap();
        assert_eq!(room.kind, ConversationKind::Voice);
        assert_eq!(room.call, CallSupport::Native);
        let (rows, _) = ctx.hub.store().inbox(Some(Network::Tiny), InboxFilter::All, 0, 20, &[]).unwrap();
        assert!(rows.iter().any(|row| row.key.id == "direct-1"));
    }

    #[test]
    fn the_composer_uses_the_servers_upload_caps() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/server ") {
                return Some(http(200, "OK", r#"{"name":"Tiny","image_max_bytes":1048576,"file_max_bytes":2097152}"#));
            }
            None
        });
        // Caps are stored only once the account is linked, which start does before refresh.
        ctx.link(protocol::Link::Offline);
        refresh(&ctx, &saved);
        assert_eq!(ctx.hub.capabilities(&ctx.account).max_file_bytes, 2_097_152);
    }

    #[test]
    fn a_message_page_is_stored_oldest_first() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/conversations/channel-1/messages?") && request.contains("limit=50") {
                return Some(http(200, "OK", &format!(
                    r#"{{"messages":[{{"id":"a-1","author_id":"{PEER}","body":"first","created_at":"2024-01-02T03:04:05Z"}},{{"id":"a-2","author_id":"{ME}","body":"second","created_at":"2024-01-02T03:05:05Z"}}]}}"#
                )));
            }
            None
        });
        load_page(&ctx, &saved, "channel-1", None, PAGE);
        let page = ctx.hub.store().messages(&ctx.key("channel-1"), &Page::Newest, 10, 1_800_000_000).unwrap().unwrap();
        assert_eq!(page.messages.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(), ["a-1", "a-2"]);
        assert_eq!(page.messages[0].text, "first");
        assert!(!page.messages[0].outgoing);
        assert!(page.messages[1].outgoing);
    }

    #[test]
    fn an_encrypted_conversation_is_labelled_its_rows_never_read_the_servers_body_and_nothing_goes_plain() {
        let posted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = posted.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/channels ") {
                return Some(http(200, "OK", r#"{"channels":[{"id":"sealed-1","name":"secret","e2ee":true},{"id":"plain-1","name":"open"}]}"#));
            }
            if request.starts_with("GET /v1/conversations/sealed-1/messages?") {
                return Some(http(200, "OK", &format!(
                    r#"{{"messages":[{{"id":"s-1","author_id":"{PEER}","body":"what the server says","e2ee":true,"kind":"message","ciphertext":"AAAA","epoch":3,"device_id":"d-1","created_at":"{WHEN}"}}]}}"#
                )));
            }
            if request.starts_with("POST /v1/conversations/") {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            None
        });
        let encryption = |id: &str| ctx.known(id).unwrap().unwrap().encryption;
        assert_eq!(encryption("sealed-1"), Encryption::EndToEnd);
        assert_eq!(encryption("plain-1"), Encryption::Plain, "a plaintext channel says so, and is never called encrypted");
        assert_eq!(encryption("direct-1"), Encryption::Plain);
        load_page(&ctx, &saved, "sealed-1", None, PAGE);
        let stored = ctx.stored_message("sealed-1", "s-1").unwrap().unwrap();
        assert_eq!(stored.text, protocol::UNDECRYPTABLE, "never a blank, and never the server's body");
        assert_eq!(stored.seal, Seal::Undecryptable);
        let outcome = post_message(&ctx, &saved, "sealed-1", "client-9", "secret words", None, &[]);
        assert_eq!(outcome.err().as_deref(), Some(NOT_SET_UP));
        assert!(!posted.load(std::sync::atomic::Ordering::SeqCst), "nothing was posted in plain text");
    }

    #[test]
    fn a_send_replaces_the_pending_row_with_the_server_id() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("POST /v1/conversations/direct-1/messages ") {
                assert!(request.contains("client-1"), "{request}");
                assert!(request.contains("hi"), "{request}");
                return Some(http(201, "Created", &format!(
                    r#"{{"id":"server-1","author_id":"{ME}","body":"hi","client_id":"client-1","created_at":"{WHEN}"}}"#
                )));
            }
            None
        });
        let mut pending = Message::plain("client-1", "Ada", "hi", 1_704_164_645, true);
        pending.sender.id = ME.to_owned();
        pending.state = SendState::Pending;
        ctx.hub.store().ingest(&ctx.key("direct-1"), &pending, false).unwrap();
        let outcome = post_message(&ctx, &saved, "direct-1", "client-1", "hi", None, &[]);
        ctx.sent("direct-1", "client-1", outcome);
        assert!(ctx.stored_message("direct-1", "client-1").unwrap().is_none());
        let stored = ctx.stored_message("direct-1", "server-1").unwrap().unwrap();
        assert_eq!(stored.text, "hi");
        assert_eq!(stored.client_id.as_deref(), Some("client-1"));
    }

    #[test]
    fn an_edit_replaces_the_text() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("PATCH /v1/messages/e-1 ") {
                return Some(http(200, "OK", &format!(
                    r#"{{"id":"e-1","author_id":"{ME}","body":"changed","created_at":"{WHEN}","edited_at":"2024-01-02T04:04:05Z"}}"#
                )));
            }
            None
        });
        let mut message = Message::plain("e-1", "Ada", "before", 1_704_164_645, true);
        message.sender.id = ME.to_owned();
        ctx.hub.store().ingest(&ctx.key("direct-1"), &message, false).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        edit(&ctx, &saved, "direct-1", "e-1", "changed", tx);
        assert_eq!(rx.recv().unwrap(), Ok(()));
        let stored = ctx.stored_message("direct-1", "e-1").unwrap().unwrap();
        assert_eq!(stored.text, "changed");
        assert!(stored.edited);
    }

    #[test]
    fn a_reaction_update_uses_the_server_count() {
        let (_adapter, ctx, saved) = fixture(|_| None);
        let mut message = Message::plain("r-1", "Ada", "hi", 1_704_164_645, true);
        message.sender.id = ME.to_owned();
        ctx.hub.store().ingest(&ctx.key("direct-1"), &message, false).unwrap();
        on_live(&ctx, &saved, &HashMap::new(), "reaction.updated", &json!({
            "conversation_id": "direct-1", "message_id": "r-1", "emoji": "ok", "on": true, "user_id": PEER, "count": 4
        }));
        assert!(ctx.stored_message("direct-1", "r-1").unwrap().unwrap().reactions.is_empty());
        on_live(&ctx, &saved, &HashMap::new(), "reaction.updated", &json!({
            "conversation_id": "direct-1", "message_id": "r-1", "emoji": "👍", "on": true, "user_id": PEER, "count": 5
        }));
        let stored = ctx.stored_message("direct-1", "r-1").unwrap().unwrap();
        assert_eq!(stored.reactions.len(), 1);
        assert_eq!(stored.reactions[0].count, 5);
        assert!(!stored.reactions[0].mine);
        on_live(&ctx, &saved, &HashMap::new(), "reaction.updated", &json!({
            "conversation_id": "direct-1", "message_id": "r-1", "emoji": "👍", "on": true, "user_id": ME, "count": 6
        }));
        let stored = ctx.stored_message("direct-1", "r-1").unwrap().unwrap();
        assert_eq!(stored.reactions[0].count, 6);
        assert!(stored.reactions[0].mine);
    }

    #[test]
    fn a_peers_read_marks_my_message_seen() {
        let (_adapter, ctx, saved) = fixture(|_| None);
        assert!(ctx.known("direct-1").unwrap().unwrap().preview_mine);
        assert!(!ctx.known("direct-1").unwrap().unwrap().seen);
        on_live(&ctx, &saved, &HashMap::new(), "read.updated", &json!({
            "conversation_id": "direct-1", "user_id": PEER, "message_id": "m-2"
        }));
        assert!(ctx.known("direct-1").unwrap().unwrap().seen);
        on_live(&ctx, &saved, &HashMap::new(), "read.updated", &json!({
            "conversation_id": "direct-1", "user_id": ME, "message_id": "m-2"
        }));
        assert_eq!(ctx.known("direct-1").unwrap().unwrap().last_read.as_deref(), Some("m-2"));
    }

    #[test]
    fn gif_and_video_attachments_keep_their_kind() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/conversations/channel-1/messages?") {
                return Some(http(200, "OK", &format!(
                    r#"{{"messages":[{{"id":"g-1","author_id":"{PEER}","body":"","created_at":"{WHEN}","attachments":[
                        {{"id":"gif-1","name":"a.gif","mime":"image/gif","kind":"gif","bytes":12,"width":10,"height":8}},
                        {{"id":"vid-1","name":"a.mp4","mime":"video/mp4","kind":"video","bytes":40,"width":100,"height":80,"duration":3}},
                        {{"id":"gif-2","name":"b.gif","mime":"image/gif","kind":"","bytes":4,"width":2,"height":2}}
                    ]}}]}}"#
                )));
            }
            None
        });
        load_page(&ctx, &saved, "channel-1", None, PAGE);
        let stored = ctx.stored_message("channel-1", "g-1").unwrap().unwrap();
        assert_eq!(stored.attachments[0].file.kind, MediaKind::Gif);
        assert_eq!(stored.attachments[1].file.kind, MediaKind::Video);
        assert_eq!(stored.attachments[1].duration, 3);
        assert_eq!(stored.attachments[1].file.width, 100);
        assert_eq!(stored.attachments[2].file.kind, MediaKind::Gif);
    }

    #[test]
    fn catalogs_are_empty_and_a_download_stays_on_this_server() {
        let base = super::super::tests::serve(|_| http(404, "Not Found", "{}"));
        let saved = saved(&base);
        let (_adapter, ctx) = open_ctx();
        let (page_tx, page_rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &Live::new().unwrap(), Command::Catalog {
            kind: CatalogKind::Gifs,
            space_id: None,
            query: String::new(),
            cursor: None,
            reply: page_tx,
        });
        assert!(page_rx.recv().unwrap().unwrap().items.is_empty());
        let (rules_tx, rules_rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &Live::new().unwrap(), Command::ReactionRules { conversation_id: "direct-1".into(), reply: rules_tx });
        let rules = rules_rx.recv().unwrap().unwrap();
        assert!(rules.offers("👍", false));
        assert!(!rules.offers("custom", true));
        let (ok_tx, ok_rx) = std::sync::mpsc::channel();
        let url = format!("{base}/v1/media/gif-1");
        serve(&ctx, &saved, &Live::new().unwrap(), Command::Download { id: "gif-1".into(), url: url.clone(), reply: ok_tx });
        let download = ok_rx.recv().unwrap().unwrap();
        assert_eq!(download.url, url);
        assert_eq!(download.headers[0].0, "Authorization");
        assert!(download.headers[0].1.starts_with("Bearer "));
        let (no_tx, no_rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &Live::new().unwrap(), Command::Download { id: "x".into(), url: "https://evil.example/v1/media/x".into(), reply: no_tx });
        assert!(no_rx.recv().unwrap().is_err());
    }

    #[test]
    fn a_signed_media_url_is_fetched_and_the_bearer_stays() {
        let saved = saved("https://chat.example");
        let wire: WireMessage = serde_json::from_value(json!({
            "id": "m-1",
            "author_id": ME,
            "body": "",
            "created_at": WHEN,
            "attachments": [{
                "id": "file-1",
                "name": "a.png",
                "mime": "image/png",
                "kind": "image",
                "url": "https://chat.example/v1/media/file-1?ex=1&is=2&hm=ab"
            }]
        }))
        .unwrap();
        let mut media = Vec::new();
        map_message(&saved, &HashMap::new(), &wire, &HashMap::new(), &mut media).unwrap();
        assert_eq!(media[0].1, "https://chat.example/v1/media/file-1?ex=1&is=2&hm=ab");
        let foreign: WireMessage = serde_json::from_value(json!({
            "id": "m-2",
            "author_id": ME,
            "attachments": [{ "id": "file-2", "url": "https://evil.example/v1/media/file-2?hm=no" }]
        }))
        .unwrap();
        let mut media = Vec::new();
        map_message(&saved, &HashMap::new(), &foreign, &HashMap::new(), &mut media).unwrap();
        assert_eq!(media[0].1, "https://chat.example/v1/media/file-2");
        let (_adapter, ctx) = open_ctx();
        let (tx, rx) = std::sync::mpsc::channel();
        let url = "https://chat.example/v1/media/file-1?ex=1&is=2&hm=ab";
        serve(&ctx, &saved, &Live::new().unwrap(), Command::Download { id: "file-1".into(), url: url.into(), reply: tx });
        let download = rx.recv().unwrap().unwrap();
        assert_eq!(download.url, url);
        assert_eq!(download.headers[0].0, "Authorization");
        assert!(download.headers[0].1.starts_with("Bearer "));
    }

    #[test]
    fn a_minted_media_path_is_joined_onto_the_server_and_keeps_the_bearer() {
        let saved = saved("https://chat.example");
        let path = "/v1/media/file-1?ex=1700086400&is=1700000000&hm=ab";
        let wire: WireMessage = serde_json::from_value(json!({
            "id": "m-1",
            "author_id": ME,
            "body": "",
            "created_at": WHEN,
            "attachments": [{
                "id": "file-1",
                "name": "a.png",
                "mime": "image/png",
                "kind": "image",
                "url": path
            }]
        }))
        .unwrap();
        let mut media = Vec::new();
        map_message(&saved, &HashMap::new(), &wire, &HashMap::new(), &mut media).unwrap();
        let joined = format!("https://chat.example{path}");
        assert_eq!(media[0].1, joined);
        let (_adapter, ctx) = open_ctx();
        let (tx, rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &Live::new().unwrap(), Command::Download { id: "file-1".into(), url: joined.clone(), reply: tx });
        let download = rx.recv().unwrap().unwrap();
        assert_eq!(download.url, joined);
        assert_eq!(download.headers[0].0, "Authorization");
        assert!(download.headers[0].1.starts_with("Bearer "));
    }

    #[test]
    fn a_blocked_media_upload_does_not_post_the_message() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let posted = Arc::new(AtomicBool::new(false));
        let bearer = Arc::new(AtomicBool::new(false));
        let (posted_flag, bearer_flag) = (posted.clone(), bearer.clone());
        let dir = std::env::temp_dir().join(format!("tiny-blocked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.pdf");
        std::fs::write(&path, b"%PDF-1.1\n").unwrap();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/server ") {
                return Some(http(200, "OK", r#"{"name":"Tiny","image_max_bytes":8388608,"file_max_bytes":26214400}"#));
            }
            if request.starts_with("POST /v1/media ") {
                if request.to_ascii_lowercase().contains("authorization: bearer t") {
                    bearer_flag.store(true, Ordering::Relaxed);
                }
                return Some(http(403, "Forbidden", r#"{"error":"blocked"}"#));
            }
            if request.starts_with("POST /v1/conversations/") {
                posted_flag.store(true, Ordering::Relaxed);
                return Some(http(201, "Created", "{}"));
            }
            None
        });
        let mut pending = Message::plain("client-9", "Ada", "hi", 1, true);
        pending.sender.id = ME.to_owned();
        pending.state = SendState::Pending;
        ctx.hub.store().ingest(&ctx.key("direct-1"), &pending, false).unwrap();
        let outgoing = Outgoing {
            files: vec![protocol::OutgoingFile {
                path: path.to_string_lossy().into_owned(),
                name: "notes.pdf".into(),
                spoiler: false,
                as_file: false,
                original: false,
            }],
            ..Outgoing::default()
        };
        send_with(&ctx, &saved, "direct-1".into(), "client-9".into(), "hi".into(), None, outgoing);
        let started = std::time::Instant::now();
        let mut failed = None;
        while started.elapsed() < std::time::Duration::from_secs(5) {
            if let Ok(Some(message)) = ctx.stored_message("direct-1", "client-9") {
                if let SendState::Failed { reason } = &message.state {
                    failed = Some(reason.clone());
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(failed.as_deref(), Some("Tiny blocked that file"));
        assert!(bearer.load(Ordering::Relaxed), "the upload keeps the bearer");
        assert!(!posted.load(Ordering::Relaxed), "a blocked upload must not post the message");
    }
}
