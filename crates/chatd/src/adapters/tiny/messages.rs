//! Text channels and direct messages on a Tiny server. Voice rooms stay voice channels.
//!
//! The shell already draws a conversation once a row is in the store, so this module only
//! maps the server's JSON into that store and answers the commands the shell already sends.
//! A GIF or a video is an attachment the existing viewer fetches. Reactions are Unicode emoji.

use super::e2ee::{self, Read};
use super::live::{Ask, Live};
use super::Saved;
use crate::adapters::{CatalogPage, Command, Context, Download, Refusal};
use crate::blobs::upload::{self, Target};
use crate::store::{ConversationRow, PersonRow, PreviewLine, SpaceRow};
use ::e2ee as e2ee_core;
use protocol::{
    join_host, Attachment, CallSupport, Capabilities, CatalogKind, ConversationKind, Encryption,
    Group, GroupKind, MediaFile, MediaKind, Message, MessageKind, Outgoing, Reaction,
    ReactionRules, ReplyTo, Seal, TINY_FEATURE_DIRECT_CALLS,
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
    // A 200 with an empty channels or directs list still runs replace_tree: every conversation
    // that list left out is deleted with its messages. A brief empty answer during a server
    // update is not an empty community. A first connect with nothing of that kind still replaces.
    let (had_channels, had_directs, had_voice) = kept_list_kinds(ctx);
    if channels.is_empty() || directs.is_empty() {
        if (channels.is_empty() && had_channels) || (directs.is_empty() && had_directs) {
            tracing::warn!("chatd: Tiny server: empty channel or direct list; keeping the stored tree");
            return;
        }
    }
    let rooms = match super::rooms_of(saved) {
        super::Listed::Rows(rows) => rows,
        super::Listed::Empty => Vec::new(),
        super::Listed::Failed(detail) => {
            tracing::warn!("chatd: Tiny server: rooms: {detail}");
            return;
        }
    };
    // Same for voice rooms: an empty rooms answer (or a 404 on an old server that never had
    // rooms) must not delete rooms already stored.
    if rooms.is_empty() && had_voice {
        tracing::warn!("chatd: Tiny server: empty rooms list; keeping the stored tree");
        return;
    }
    let callable = directs_callable(saved);
    let mut people: Vec<PersonRow> = names
        .iter()
        .filter(|(id, _)| super::safe_id(id))
        .map(|(id, name)| PersonRow {
            id: id.clone(),
            name: name.clone(),
            ..PersonRow::default()
        })
        .collect();
    let mut groups = Vec::new();
    let mut rows = Vec::new();
    let mut noted = Vec::new();
    let mut categories = HashSet::new();
    for channel in &channels {
        if !super::safe_id(&channel.id) {
            continue;
        }
        let mut row = ConversationRow::new(
            &channel.id,
            ConversationKind::Channel,
            title_or(&channel.name, &channel.id),
        );
        row.space = Some(SPACE.to_owned());
        row.topic = channel.topic.clone();
        row.position = channel.position;
        let (e2ee, history) =
            sealed_state(saved, &channel.id, channel.e2ee, channel.plaintext_history);
        row.encryption = encryption_of(saved, &channel.id, e2ee, history);
        if let Some(category) = channel
            .category_id
            .as_deref()
            .filter(|id| super::safe_id(id))
        {
            row.group = Some(category.to_owned());
            if categories.insert(category.to_owned()) {
                let position = channels
                    .iter()
                    .filter(|other| other.category_id.as_deref() == Some(category))
                    .map(|other| other.position)
                    .min()
                    .unwrap_or(0);
                let name = channel
                    .category_name
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(category);
                groups.push(Group::new(category, name, position));
            }
        }
        noted.push(
            Note::from_last(
                &channel.id,
                channel.unread,
                None,
                None,
                channel.last_message.as_ref(),
                false,
            )
            .sealed(channel.e2ee),
        );
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
            people.push(PersonRow {
                id: direct.peer.user_id.clone(),
                name: name.clone(),
                ..PersonRow::default()
            });
        }
        let mut row = ConversationRow::new(&direct.id, ConversationKind::Direct, name);
        row.space = Some(SPACE.to_owned());
        row.group = Some("directs".to_owned());
        row.peer = Some(direct.peer.user_id.clone());
        row.position = DIRECTS_AT + index as i32;
        if callable {
            row.call = CallSupport::Native;
        }
        let (e2ee, history) =
            sealed_state(saved, &direct.id, direct.e2ee, direct.plaintext_history);
        row.encryption = encryption_of(saved, &direct.id, e2ee, history);
        let mine = direct
            .last_message
            .as_ref()
            .is_some_and(|last| last.author_id == saved.user_id);
        noted.push(
            Note::from_last(
                &direct.id,
                direct.unread,
                direct.last_read.as_deref(),
                direct.peer_read.as_deref(),
                direct.last_message.as_ref(),
                mine,
            )
            .sealed(direct.e2ee),
        );
        rows.push(row);
    }
    if !rooms.is_empty() {
        groups.push(Group::new("rooms", "Rooms", ROOMS_AT));
    }
    for (index, room) in rooms.iter().enumerate() {
        if !super::safe_id(&room.id) {
            continue;
        }
        let mut row = ConversationRow::new(
            &room.id,
            ConversationKind::Voice,
            title_or(&room.name, &room.id),
        );
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
    for direct in &directs {
        if let Some(call) = &direct.last_call {
            record_call(ctx, saved, names, &direct.id, call);
        }
    }
}

/// The server lists direct calls. A server not yet checked is asked; a 404 is an older one.
fn directs_callable(saved: &Saved) -> bool {
    let known = super::manage::known(saved);
    let capabilities = if known.checked {
        known.capabilities
    } else {
        super::manage::capabilities_of(saved)
    };
    capabilities.has(TINY_FEATURE_DIRECT_CALLS)
}

/// A durable frame, or one of the message frames a server sent without a seq.
pub(crate) fn on_live(
    ctx: &Context,
    saved: &Saved,
    names: &HashMap<String, String>,
    kind: &str,
    frame: &Value,
) {
    match kind {
        "channel.created" | "channel.updated" => channel_upsert(ctx, saved, names, frame),
        "channel.deleted" => {
            if let Some(id) = nested_id(frame, "channel", "channel_id") {
                let _ = ctx.conversation_gone(id);
            }
        }
        "direct.created" => direct_upsert(ctx, saved, names, frame),
        "direct.call" => {
            if let Ok(call) = serde_json::from_value::<WireCall>(frame.clone()) {
                record_call(ctx, saved, names, &call.direct_id, &call);
            }
        }
        "e2ee.upgraded" => upgraded(ctx, saved, frame),
        "message.created" | "message.edited" => {
            if let Some((conversation, wire)) = framed_message(frame) {
                if is_self(saved, &conversation) {
                    // Off the socket's thread: it may answer, and read conversations again.
                    let (ctx, saved) = (ctx.clone(), saved.clone());
                    let _ = std::thread::Builder::new().name("tiny-e2ee-siblings".into()).spawn(move || from_sibling(&ctx, &saved, &wire));
                } else {
                    deliver(ctx, saved, names, &conversation, &wire);
                }
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
        // The sealed kind: a reaction blob in an encrypted conversation, opaque to the server.
        // Off the socket's thread: it asks the server for the list behind it.
        "e2ee.reaction" => {
            let conversation_id = field(frame, "conversation_id");
            if super::safe_id(conversation_id) {
                if let Some(device) = e2ee::started(saved) {
                    let (conversation_id, frame, ctx) = (conversation_id.to_owned(), frame.clone(), ctx.clone());
                    let _ = std::thread::Builder::new().name("tiny-e2ee".into()).spawn(move || {
                        for landed in device.lock().unwrap_or_else(|err| err.into_inner()).heard_reaction(&conversation_id, &frame) {
                            apply_reaction(&ctx, &conversation_id, &landed);
                        }
                    });
                }
            }
        }
        "read.updated" => read_updated(ctx, saved, frame),
        "server.updated" => ctx.capabilities(account_caps(saved)),
        // The group of an encrypted conversation changed, this device's key packages ran low, or
        // someone's devices changed. Off the socket's thread: it asks the server. A row that did
        // not open for want of the group is read again once the group is here.
        "e2ee.handshake" | "e2ee.keys_low" | "e2ee.device" => {
            if let Some(device) = e2ee::started(saved) {
                let (kind, frame, ctx) = (kind.to_owned(), frame.clone(), ctx.clone());
                let _ = std::thread::Builder::new().name("tiny-e2ee".into()).spawn(move || {
                    let noticed = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).heard(&kind, &frame);
                    // "New device added": the page shows it, on every device of the person.
                    if noticed {
                        ctx.community_changed();
                    }
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
            | "direct.call"
            | "e2ee.upgraded"
            | "message.created"
            | "message.edited"
            | "message.deleted"
            | "reaction.updated"
            | "e2ee.reaction"
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
        Command::History {
            conversation_id,
            before,
        } => load_page(ctx, saved, &conversation_id, Some(before), PAGE),
        Command::HistoryAfter {
            conversation_id, ..
        } => load_page(ctx, saved, &conversation_id, None, PAGE),
        Command::HistoryAround {
            conversation_id,
            around,
        } => {
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
        Command::Send { conversation_id, client_id, text, reply_to } => send(ctx, saved, conversation_id, client_id, text, reply_to),
        Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
            send_with(ctx, saved, conversation_id, client_id, text, reply_to, *with);
        }
        Command::CancelSend { client_id, .. } => upload::cancel(&ctx.account, &client_id),
        Command::React { conversation_id, message_id, emoji, on, reply } => react(ctx, saved, &conversation_id, &message_id, &emoji, on, reply),
        Command::ReactionRules { reply, .. } => {
            let _ = reply.send(Ok(ReactionRules::default()));
        }
        Command::Edit {
            conversation_id,
            message_id,
            text,
            reply,
        } => edit(ctx, saved, &conversation_id, &message_id, &text, reply),
        Command::Delete {
            conversation_id,
            message_id,
            reply,
        } => delete(ctx, saved, &conversation_id, &message_id, reply),
        Command::Typing(conversation_id) => {
            if super::safe_id(&conversation_id) {
                live.ask(Ask::Type { conversation_id });
            }
        }
        Command::StartDirect { person_id, reply } => start_direct(ctx, saved, &person_id, reply),
        Command::StartDirectCall { person_id, reply } => start_direct_intent(ctx, saved, &person_id, true, reply),
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
                let sealed = e2ee::started(saved).and_then(|device| {
                    device
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .sealed_file(&id)
                });
                let _ = reply.send(Ok(Download {
                    url,
                    headers: vec![(
                        "Authorization".to_owned(),
                        format!("Bearer {}", saved.token),
                    )],
                    sealed,
                }));
            } else {
                let _ = reply.send(Err(Refusal::Refused(
                    "That file is on another server.".to_owned(),
                )));
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
                if self.sealed {
                    rehydrate_opened(ctx, saved, &self.id, last, self.mine, names);
                }
                let text = if self.sealed {
                    sealed_preview(ctx, saved, &self.id, &last.id)
                } else {
                    last.body.clone()
                };
                let _ = ctx.preview(&self.id, &PreviewLine {
                    id: last.id.clone(),
                    text,
                    sender: if self.mine { String::new() } else { name_of(names, saved, &last.author_id) },
                    mine: self.mine,
                    sent: rfc3339(&last.created_at).unwrap_or(updated),
                    expires_at: None,
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
    row.encryption = encryption_of(saved, &channel.id, channel.e2ee, channel.plaintext_history);
    if ctx.conversation(&row).is_err() {
        return;
    }
    // The broadcast copies the actor's unread into the channel object. Ours stays until our
    // own list (`publish`) says otherwise, or a live message moves it.
    let unread = match ctx.known(&channel.id) {
        Ok(Some(existing)) => existing.unread,
        _ => channel.unread,
    };
    Note::from_last(&channel.id, unread, None, None, channel.last_message.as_ref(), false).sealed(channel.e2ee).apply(ctx, saved, names);
}

fn direct_upsert(ctx: &Context, saved: &Saved, names: &HashMap<String, String>, frame: &Value) {
    let Some(mut value) = nested_value(frame, "direct") else {
        return;
    };
    // Live events name the pair; REST names the caller's peer. Validate the pair before
    // accepting either shape, including an event that also happens to carry a peer field.
    let paired = value.get("low_user_id").is_some() || value.get("high_user_id").is_some();
    if paired {
        let (Some(low), Some(high)) = (value.get("low_user_id").and_then(Value::as_str), value.get("high_user_id").and_then(Value::as_str)) else {
            return;
        };
        if !super::safe_id(low) || !super::safe_id(high) || low == high {
            return;
        }
        let peer = if low == saved.user_id { high } else if high == saved.user_id { low } else { return };
        let name = names.get(peer).filter(|name| !name.is_empty()).cloned().or_else(|| {
            ctx.hub.store().person(&ctx.key(peer)).ok().flatten().map(|person| person.name)
        }).unwrap_or_else(|| "Someone".to_owned());
        value["peer"] = json!({"user_id": peer, "display_name": name});
    }
    let Ok(direct) = serde_json::from_value::<WireDirect>(value) else {
        return;
    };
    if !super::safe_id(&direct.id) || !super::safe_id(&direct.peer.user_id) || direct.peer.user_id == saved.user_id {
        return;
    }
    let name = title_or(&direct.peer.display_name, "Someone");
    let _ = ctx.people(&[PersonRow {
        id: direct.peer.user_id.clone(),
        name: name.clone(),
        ..PersonRow::default()
    }]);
    let mut group = Group::new("directs", "Direct messages", DIRECTS_AT);
    group.kind = GroupKind::Directs;
    let _ = ctx.group(SPACE, &group);
    let mut row = ConversationRow::new(&direct.id, ConversationKind::Direct, name);
    row.space = Some(SPACE.to_owned());
    row.group = Some("directs".to_owned());
    row.peer = Some(direct.peer.user_id.clone());
    row.position = DIRECTS_AT;
    row.call = if directs_callable(saved) { CallSupport::Native } else { CallSupport::None };
    // A replayed creation predates any later upgrade. It can add encryption, never remove it.
    let kept = if paired { ctx.known(&direct.id).ok().flatten().map(|row| row.encryption) } else { None };
    let sealed = direct.e2ee || kept.is_some_and(|encryption| encryption.is_sealed());
    let history = direct.plaintext_history || kept == Some(Encryption::EndToEndAfterPlain);
    row.encryption = encryption_of(saved, &direct.id, sealed, history);
    if ctx.conversation(&row).is_err() {
        return;
    }
    // A creation event carries no viewer counts or message preview. Replaying it must
    // leave messages already heard intact; only the REST shape supplies a full snapshot.
    if !paired {
        let mine = direct.last_message.as_ref().is_some_and(|last| last.author_id == saved.user_id);
        Note::from_last(&direct.id, direct.unread, direct.last_read.as_deref(), direct.peer_read.as_deref(), direct.last_message.as_ref(), mine).sealed(direct.e2ee).apply(ctx, saved, names);
    }
}

fn deliver(ctx: &Context, saved: &Saved, names: &HashMap<String, String>, conversation_id: &str, wire: &WireMessage) {
    if !super::safe_id(conversation_id) {
        return;
    }
    let mut media = Vec::new();
    let opened = open_rows(ctx, saved, conversation_id, &[wire]);
    if wire.kind.as_deref() == Some("edit") {
        // An encrypted edit is a row of its own: what it changed is its target.
        refold(ctx, saved, names, conversation_id, &opened);
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
fn refold(ctx: &Context, saved: &Saved, names: &HashMap<String, String>, conversation_id: &str, opened: &HashMap<String, Read>) {
    for (id, read) in opened {
        let Read::Opened(opened) = read else {
            continue;
        };
        let Ok(Some(mut message)) = ctx.stored_message(conversation_id, id) else {
            continue;
        };
        let Some(auth) = &opened.auth else { continue; };
        let author = auth.author.to_string();
        message.sender.id = author.clone();
        message.sender.name = name_of(names, saved, &author);
        message.sender.avatar = None;
        message.outgoing = author == saved.user_id;
        message.saved_history = false;
        message.text = opened.text.clone();
        message.edited = true;
        relink(&mut message);
        let mut media = Vec::new();
        message.attachments = sealed_attachments(saved, opened, &mut media);
        if !media.is_empty() {
            let _ = ctx.media(&media);
        }
        let _ = ctx.message(conversation_id, &message);
    }
}

/// A row that carries ciphertext is sealed even when the payload omits `e2ee`. Treating it as
/// plaintext stores the empty body, and the pane draws no words.
fn row_is_sealed(wire: &WireMessage) -> bool {
    wire.e2ee || wire.ciphertext.as_ref().is_some_and(|text| !text.is_empty())
}

/// What the end-to-end encrypted rows of a page say. Empty for a plaintext page. When the MLS
/// device is not started yet, still returns what this device already opened from the sealed
/// cache so a fetch right after an update does not blank chats with undecryptable stubs.
fn open_rows(ctx: &Context, saved: &Saved, conversation_id: &str, wires: &[&WireMessage]) -> HashMap<String, Read> {
    if !wires.iter().any(|wire| row_is_sealed(wire)) {
        return HashMap::new();
    }
    let Some(device) = e2ee::started(saved) else {
        let mut out = HashMap::new();
        for wire in wires.iter().filter(|wire| row_is_sealed(wire) && !present(&wire.deleted_at)) {
            let Some(opened) = e2ee::opened_row(saved, conversation_id, &wire.id) else {
                continue;
            };
            if opened.edits.is_some() {
                out.insert(wire.id.clone(), Read::Edit);
            } else {
                out.insert(wire.id.clone(), Read::Opened(opened));
            }
        }
        return out;
    };
    let rows: Vec<e2ee::Row> = wires
        .iter()
        .filter(|wire| row_is_sealed(wire) && !present(&wire.deleted_at))
        .map(|wire| e2ee::Row {
            id: &wire.id,
            author_id: &wire.author_id,
            kind: wire.kind.as_deref().unwrap_or("message"),
            ciphertext: wire.ciphertext.as_deref(),
            epoch: wire.epoch.unwrap_or(0),
            generation: wire.generation,
            group_id: wire.group_id.as_deref(),
            device_id: wire.device_id.as_deref().unwrap_or(""),
            target_id: wire.target_id.as_deref(),
            client_id: wire.client_id.as_deref(),
            created_at: &wire.created_at,
        })
        .collect();
    let saved_rows: HashMap<String, Message> = wires.iter().take(PAGE_MAX as usize)
        .filter_map(|wire| ctx.stored_message(conversation_id, &wire.id).ok().flatten().map(|message| (wire.id.clone(), message))).collect();
    let read = {
        let mut device = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        device.note_encrypted(conversation_id, true);
        let mut read = device.read(conversation_id, &rows);
        for (id, read) in &mut read {
            if let Read::Legacy(opened) = read {
                if let Some(saved) = saved_rows.get(id) {
                    if saved.id == *id && opened.keep_saved_attribution(saved) { device.remember(conversation_id, id, opened.clone()); }
                }
            }
        }
        read
    };
    // What did not open is asked of the person's other devices, off this thread.
    e2ee::siblings::ask_soon(device);
    read
}

/// A message of the person's own conversation: what another of their devices said, never a
/// message to show. Conversations that gained messages from it are read again.
fn from_sibling(ctx: &Context, saved: &Saved, wire: &WireMessage) {
    let Some(device) = e2ee::started(saved) else {
        return;
    };
    let row = e2ee::Row {
        id: &wire.id,
        author_id: &wire.author_id,
        kind: wire.kind.as_deref().unwrap_or("message"),
        ciphertext: wire.ciphertext.as_deref(),
        epoch: wire.epoch.unwrap_or(0),
            generation: wire.generation,
            group_id: wire.group_id.as_deref(),
        device_id: wire.device_id.as_deref().unwrap_or(""),
        target_id: wire.target_id.as_deref(),
        client_id: wire.client_id.as_deref(),
        created_at: &wire.created_at,
    };
    let (gained, changed) = {
        let mut device = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let gained = device.sibling_said(&row);
        (gained, device.sibling_notice())
    };
    if changed { ctx.community_changed(); }
    for conversation in gained {
        load_page(ctx, saved, &conversation, None, PAGE_MAX);
    }
}

fn is_self(saved: &Saved, conversation_id: &str) -> bool {
    e2ee::started(saved).is_some_and(|device| device.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_self(conversation_id))
}

/// The conversation's label. An upgrade that left older messages in the clear says so.
fn encryption_of(saved: &Saved, conversation_id: &str, e2ee: bool, history: bool) -> Encryption {
    if let Some(device) = e2ee::started(saved) {
        device.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).note_encrypted(conversation_id, e2ee);
    }
    if e2ee && history {
        Encryption::EndToEndAfterPlain
    } else if e2ee {
        Encryption::EndToEnd
    } else {
        Encryption::Plain
    }
}

/// What the list says, after asking the server to encrypt a plaintext conversation this device
/// can already send in. A server that has encryption off, or a conversation that cannot take
/// the group, is left as it is.
fn sealed_state(saved: &Saved, id: &str, e2ee: bool, history: bool) -> (bool, bool) {
    if e2ee {
        return (true, history);
    }
    upgrade_plaintext(saved, id).unwrap_or((false, false))
}

fn upgrade_skipped(id: &str) -> bool {
    skipped_upgrades().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).contains(id)
}

fn skip_upgrade(id: &str) {
    skipped_upgrades().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(id.to_owned());
}

fn skipped_upgrades() -> &'static std::sync::Mutex<HashSet<String>> {
    static SKIPPED: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    SKIPPED.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// Asks the server to encrypt `id`. `Some` is what it became. `None` leaves the plaintext row.
fn upgrade_plaintext(saved: &Saved, id: &str) -> Option<(bool, bool)> {
    if !super::safe_id(id) || upgrade_skipped(id) {
        return None;
    }
    let Some(shared) = e2ee::started(saved) else {
        return None;
    };
    let ready = {
        let device = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mode = device.mode();
        device.ready() && !mode.is_empty() && mode != "off"
    };
    if !ready {
        return None;
    }
    match super::asked("POST", saved, &format!("/v1/conversations/{id}/e2ee"), None) {
        Ok((200, body)) => {
            let e2ee = body.get("e2ee").and_then(Value::as_bool).unwrap_or(false);
            let history = body.get("plaintext_history").and_then(Value::as_bool).unwrap_or(false);
            if e2ee {
                let id = id.to_owned();
                let _ = std::thread::Builder::new().name("tiny-e2ee".into()).spawn(move || {
                    if let Err(why) = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).sync(&id) {
                        tracing::warn!("chatd: Tiny e2ee: {id}: {why}");
                    }
                });
            }
            Some((e2ee, history))
        }
        Ok((status, body)) => {
            let error = body.get("error").and_then(Value::as_str).unwrap_or("");
            if error == "e2ee_group_too_large" || error == "e2ee_twin" {
                skip_upgrade(id);
            }
            tracing::warn!("chatd: Tiny server: encrypt {id}: {status} {error}");
            None
        }
        Err(detail) => {
            tracing::warn!("chatd: Tiny server: encrypt {id}: {detail}");
            None
        }
    }
}

/// The other people's clients hear that a conversation they already have is encrypted now.
fn upgraded(ctx: &Context, saved: &Saved, frame: &Value) {
    let id = field(frame, "conversation_id");
    if !super::safe_id(id) {
        return;
    }
    let history = frame.get("plaintext_history").and_then(Value::as_bool).unwrap_or(false);
    let encryption = encryption_of(saved, id, true, history);
    let _ = ctx.mark_encryption(id, encryption);
    if let Some(shared) = e2ee::started(saved) {
        let id = id.to_owned();
        let _ = std::thread::Builder::new().name("tiny-e2ee".into()).spawn(move || {
            if let Err(why) = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).sync(&id) {
                tracing::warn!("chatd: Tiny e2ee: {id}: {why}");
            }
        });
    }
}

/// When sqlite lost a sealed row but the sealed cache still has it, put that opened value back
/// into the store so the pane is not empty for the last line. Full older history still needs a
/// fetch; this restores what the list already names, including captionless attachments.
fn rehydrate_opened(
    ctx: &Context,
    saved: &Saved,
    conversation_id: &str,
    last: &WireLast,
    mine: bool,
    names: &HashMap<String, String>,
) {
    if ctx
        .stored_message(conversation_id, &last.id)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    let Some(opened) = e2ee::opened_row(saved, conversation_id, &last.id) else {
        return;
    };
    let mut media = Vec::new();
    let attachments = sealed_attachments(saved, &opened, &mut media);
    if opened.text.trim().is_empty() && attachments.is_empty() {
        return;
    }
    let sender = if mine {
        String::new()
    } else {
        name_of(names, saved, &last.author_id)
    };
    let sent = rfc3339(&last.created_at).unwrap_or(0);
    let mut message = Message::plain(&last.id, &sender, &opened.text, sent, mine);
    message.sender.id = last.author_id.clone();
    message.seal = Seal::Encrypted;
    message.attachments = attachments;
    relink(&mut message);
    if !media.is_empty() {
        let _ = ctx.media(&media);
    }
    let _ = ctx.history(conversation_id, &[message]);
}

/// The last line of an encrypted conversation, from what this device opened, or a word that says
/// there is one. An Encrypted row with no opened text is not a blank inbox line. When the sqlite
/// row was wiped but the sealed cache still holds the opened text, that text is the preview.
fn sealed_preview(ctx: &Context, saved: &Saved, conversation_id: &str, message_id: &str) -> String {
    match ctx.stored_message(conversation_id, message_id) {
        Ok(Some(message)) => sealed_preview_words(message.seal, &message.text),
        _ => match e2ee::opened_text(saved, conversation_id, message_id) {
            Some(text) => sealed_preview_words(Seal::Encrypted, &text),
            None => sealed_preview_words(Seal::Encrypted, ""),
        },
    }
}

fn sealed_preview_words(seal: Seal, text: &str) -> String {
    if seal == Seal::Encrypted && !text.trim().is_empty() {
        text.to_owned()
    } else {
        "Encrypted message".to_owned()
    }
}

/// The lab's sealed-preview proof. An encrypted row with no opened text still names the preview.
pub(super) fn prove_sealed_preview() -> Result<(), String> {
    let words = sealed_preview_words(Seal::Encrypted, "");
    if words == "Encrypted message" {
        Ok(())
    } else {
        Err(format!("empty encrypted preview was {words:?}, not Encrypted message"))
    }
}

/// The lab's keycap-reaction proof. A keycap is taken as a reaction, and a bare digit still is not.
pub(super) fn prove_keycap_reaction() -> Result<(), String> {
    for keycap in ["1\u{FE0F}\u{20E3}", "0\u{20E3}", "#\u{FE0F}\u{20E3}"] {
        if !unicode_emoji(keycap) {
            return Err(format!("the keycap {keycap:?} was refused as a reaction"));
        }
    }
    for not_one in ["1", "10\u{FE0F}\u{20E3}", "1\u{FE0F}", ":one:"] {
        if unicode_emoji(not_one) {
            return Err(format!("{not_one:?} was taken as a reaction"));
        }
    }
    Ok(())
}

/// The lab's sealed-preview-attachments proof. A wiped sqlite row still restores a captionless
/// image and a captioned attachment from the authenticated sealed cache. The lab sets
/// `TINYCLIENT_PROVE=sealed-preview-attachments`.
pub(super) fn prove_sealed_preview_attachments() -> Result<(), String> {
    prove_rehydrate_attachment("", "dddddddd-dddd-4ddd-8ddd-dddddddddddd", "photo.png")?;
    prove_rehydrate_attachment(
        "with a caption",
        "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
        "shot.jpg",
    )?;
    // Text-only rehydrate stays green beside the attachment cases.
    prove_sealed_cache_preview()
}

fn prove_rehydrate_attachment(text: &str, file_id: &str, name: &str) -> Result<(), String> {
    let secrets = e2ee::Secrets::generate().map_err(|err| err.to_string())?;
    let saved = Saved {
        base_url: "http://127.0.0.1:9".to_owned(),
        token: "t".to_owned(),
        user_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
        email: None,
        display_name: "Ada".to_owned(),
        role: "owner".to_owned(),
        plain_http_ok: false,
        e2ee: Some(secrets),
    };
    let conversation = "c0c0c0c0-c0c0-4c0c-8c0c-c0c0c0c0c0c0";
    let message_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let mime = if name.ends_with(".jpg") {
        "image/jpeg"
    } else {
        "image/png"
    };
    e2ee::write_opened_row(
        &saved,
        conversation,
        message_id,
        e2ee::Opened {
            text: text.to_owned(),
            attachments: vec![e2ee::OpenedFile {
                id: file_id.to_owned(),
                key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned(),
                name: name.to_owned(),
                mime: mime.to_owned(),
                bytes: 42,
                sha256: "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=".to_owned(),
                width: 640,
                height: 480,
            }],
            ..e2ee::Opened::default()
        },
    )?;
    let hub = std::sync::Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = protocol::AccountKey {
        network: protocol::Network::Tiny,
        account: saved.user_id.clone(),
    };
    let (_adapter, ctx, _inbox) = crate::adapters::local(account.clone(), hub);
    let mut row = crate::store::ConversationRow::new(conversation, ConversationKind::Direct, "Bea");
    row.encryption = Encryption::EndToEnd;
    ctx.hub
        .store()
        .upsert_conversation(&account, &row)
        .map_err(|err| err.to_string())?;
    let last = WireLast {
        id: message_id.to_owned(),
        author_id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc".to_owned(),
        body: String::new(),
        created_at: "2024-01-02T03:04:05Z".to_owned(),
    };
    let names = HashMap::new();
    rehydrate_opened(&ctx, &saved, conversation, &last, false, &names);
    let message = match ctx.stored_message(conversation, message_id).map_err(|err| err.to_string())? {
        Some(message) => message,
        None => {
            return Err(format!(
                "sealed cache did not rehydrate attachment row (text={text:?})"
            ))
        }
    };
    if message.seal != Seal::Encrypted {
        return Err(format!("rehydrated seal was {:?}, not Encrypted", message.seal));
    }
    if message.text != text {
        return Err(format!(
            "rehydrated text was {:?}, not {text:?}",
            message.text
        ));
    }
    if message.attachments.len() != 1 {
        return Err(format!(
            "rehydrated attachments were {}, not 1 (text={text:?})",
            message.attachments.len()
        ));
    }
    let attachment = &message.attachments[0];
    if attachment.file.id != file_id
        || attachment.name != name
        || attachment.mime != mime
        || attachment.file.kind != MediaKind::Image
        || attachment.file.width != 640
        || attachment.file.height != 480
        || attachment.file.bytes != 42
    {
        return Err(format!(
            "rehydrated attachment metadata was wrong: id={} name={} mime={} kind={:?} {}x{} bytes={}",
            attachment.file.id,
            attachment.name,
            attachment.mime,
            attachment.file.kind,
            attachment.file.width,
            attachment.file.height,
            attachment.file.bytes
        ));
    }
    let registered = ctx
        .hub
        .store()
        .media(&account, &[file_id.to_owned()])
        .map_err(|err| err.to_string())?;
    if registered.len() != 1 {
        return Err(format!(
            "rehydrated media was not registered (got {})",
            registered.len()
        ));
    }
    let expected_url = format!("http://127.0.0.1:9/v1/media/{file_id}");
    if registered[0].1 != expected_url {
        return Err(format!(
            "registered media url was {:?}, not {expected_url}",
            registered[0].1
        ));
    }
    Ok(())
}

/// The lab's sealed-cache-preview proof. A wiped sqlite row still names the inbox from the sealed
/// cache, and the last opened line is written back into the store. The lab sets
/// `TINYCLIENT_PROVE=sealed-cache-preview`.
pub(super) fn prove_sealed_cache_preview() -> Result<(), String> {
    let secrets = e2ee::Secrets::generate().map_err(|err| err.to_string())?;
    let saved = Saved {
        base_url: "http://127.0.0.1:9".to_owned(),
        token: "t".to_owned(),
        user_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
        email: None,
        display_name: "Ada".to_owned(),
        role: "owner".to_owned(),
        plain_http_ok: false,
        e2ee: Some(secrets),
    };
    let conversation = "c0c0c0c0-c0c0-4c0c-8c0c-c0c0c0c0c0c0";
    let message_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    e2ee::write_opened_cache(&saved, conversation, message_id, "still here")?;
    let hub = std::sync::Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = protocol::AccountKey {
        network: protocol::Network::Tiny,
        account: saved.user_id.clone(),
    };
    let (_adapter, ctx, _inbox) = crate::adapters::local(account.clone(), hub);
    let mut row = crate::store::ConversationRow::new(conversation, ConversationKind::Direct, "Bea");
    row.encryption = Encryption::EndToEnd;
    ctx.hub
        .store()
        .upsert_conversation(&account, &row)
        .map_err(|err| err.to_string())?;
    let last = WireLast {
        id: message_id.to_owned(),
        author_id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc".to_owned(),
        body: String::new(),
        created_at: "2024-01-02T03:04:05Z".to_owned(),
    };
    let names = HashMap::new();
    rehydrate_opened(&ctx, &saved, conversation, &last, false, &names);
    let words = sealed_preview(&ctx, &saved, conversation, message_id);
    if words != "still here" {
        return Err(format!("wiped-store preview was {words:?}, not still here"));
    }
    match ctx.stored_message(conversation, message_id).map_err(|err| err.to_string())? {
        Some(message) if message.text == "still here" && message.seal == Seal::Encrypted => Ok(()),
        Some(message) => Err(format!(
            "rehydrated row was {:?} / {:?}",
            message.seal, message.text
        )),
        None => Err("sealed cache did not rehydrate the store row".to_owned()),
    }
}

/// The lab's cache-open-without-device proof. A history fetch before the MLS device is started
/// still restores opened text from the sealed cache, so a wipe-and-update does not fill the
/// pane with "Can't be decrypted" stubs. The lab sets `TINYCLIENT_PROVE=cache-open-without-device`.
pub(super) fn prove_cache_open_without_device() -> Result<(), String> {
    prove_cache_recovery(false)
}

/// A device whose capabilities check cannot enrol still reads its previously opened cache.
pub(super) fn prove_cache_before_enrol() -> Result<(), String> {
    prove_cache_recovery(true)
}

fn prove_cache_recovery(start_device: bool) -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let base = format!("http://{}", listener.local_addr().map_err(|err| err.to_string())?);
    let conversation = "c0c0c0c0-c0c0-4c0c-8c0c-c0c0c0c0c0c0";
    let message_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let peer = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let sealed = format!(
        r#"{{"messages":[{{"id":"{message_id}","author_id":"{peer}","body":"","e2ee":true,"ciphertext":"AAAA","epoch":3,"device_id":"d-1","created_at":"2024-01-02T03:04:05Z"}}]}}"#
    );
    std::thread::spawn(move || answer_cache_open_probe(listener, sealed));
    let secrets = e2ee::Secrets::generate().map_err(|err| err.to_string())?;
    let saved = Saved {
        base_url: base,
        token: "t".to_owned(),
        user_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
        email: None,
        display_name: "Ada".to_owned(),
        role: "owner".to_owned(),
        plain_http_ok: false,
        e2ee: Some(secrets),
    };
    e2ee::write_opened_cache(&saved, conversation, message_id, "still here")?;
    if start_device {
        // The fixture refuses capabilities, leaving an existing device without an engine.
        e2ee::begin(&saved, std::sync::mpsc::channel().0);
    } else if e2ee::started(&saved).is_some() {
        return Err("the MLS device was started; this prove needs only the sealed cache".to_owned());
    }
    let hub = std::sync::Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = protocol::AccountKey {
        network: protocol::Network::Tiny,
        account: saved.user_id.clone(),
    };
    let (_adapter, ctx, _inbox) = crate::adapters::local(account.clone(), hub);
    let mut row = crate::store::ConversationRow::new(conversation, ConversationKind::Direct, "Bea");
    row.encryption = Encryption::EndToEnd;
    ctx.hub
        .store()
        .upsert_conversation(&account, &row)
        .map_err(|err| err.to_string())?;
    load_page(&ctx, &saved, conversation, None, PAGE);
    if e2ee::started(&saved).is_some() != start_device {
        return Err("fetch changed whether the MLS device was started".to_owned());
    }
    match ctx
        .stored_message(conversation, message_id)
        .map_err(|err| err.to_string())?
    {
        Some(stored) if stored.text == "still here" && stored.seal == Seal::Encrypted => Ok(()),
        Some(stored) => Err(format!(
            "cache hit after fetch became {:?} / {:?}",
            stored.seal, stored.text
        )),
        None => Err("cache hit was not written into the store".to_owned()),
    }
}

fn answer_cache_open_probe(listener: std::net::TcpListener, sealed: String) {
    let _ = listener.set_nonblocking(true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut got = [0u8; 2048];
                let read = std::io::Read::read(&mut stream, &mut got).unwrap_or(0);
                let request = String::from_utf8_lossy(&got[..read]);
                let (status, payload) = if request.contains("/messages") {
                    ("200 OK", sealed.as_str())
                } else {
                    ("404 Not Found", "{}")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            Err(_) => break,
        }
    }
}

/// Previously opened text and captionless media survive a fetch that cannot decrypt them.
pub(super) fn prove_keep_opened_sealed() -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let base = format!("http://{}", listener.local_addr().map_err(|err| err.to_string())?);
    std::thread::spawn(move || answer_keep_opened_probe(listener));
    let saved = Saved {
        base_url: base,
        token: "t".to_owned(),
        user_id: "11111111-1111-1111-1111-111111111111".to_owned(),
        email: None,
        display_name: "Ada".to_owned(),
        role: "owner".to_owned(),
        plain_http_ok: false,
        e2ee: None,
    };
    let hub = std::sync::Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = protocol::AccountKey {
        network: protocol::Network::Tiny,
        account: saved.user_id.clone(),
    };
    let (_adapter, ctx, _inbox) = crate::adapters::local(account, hub);
    for media_only in [false, true] {
        let mut message = Message::plain("s-keep", "Bea", if media_only { "" } else { "opened words" }, 1_700_000_000, false);
        message.sender.id = "22222222-2222-2222-2222-222222222222".to_owned();
        message.seal = Seal::Encrypted;
        if media_only {
            message.attachments.push(protocol::Attachment::new(
                "picture.png", MediaFile::new("opened-picture", protocol::MediaKind::Image),
            ));
        }
        ctx.hub.store().ingest(&ctx.key("direct-1"), &message, false)
            .map_err(|err| err.to_string())?;
        load_page(&ctx, &saved, "direct-1", None, PAGE);
        let stored = ctx.stored_message("direct-1", "s-keep")
            .map_err(|err| err.to_string())?.ok_or("opened row was deleted")?;
        if stored.text != message.text || stored.seal != Seal::Encrypted || stored.attachments != message.attachments {
            return Err(format!("opened row lost content after decrypt failure (media_only={media_only}, seal={:?})", stored.seal));
        }
    }
    Ok(())
}

/// Opening a Tiny chat reaches the real adapter command loop and fetches its history.
pub(super) fn prove_tiny_open_backfill() -> Result<(), String> {
    const ME: &str = "11111111-1111-1111-1111-111111111111";
    const PEER: &str = "22222222-2222-2222-2222-222222222222";
    let base = super::serve(|request| {
        let body = if request.starts_with("GET /v1/conversations/direct-1/messages?") {
            json!({"messages":[{"id":"m-backfill","author_id":PEER,"body":"fetched after open","created_at":"2024-01-02T03:04:05Z"}]})
        } else if request.starts_with("GET /v1/channels ") {
            json!({"channels":[]})
        } else if request.starts_with("GET /v1/directs ") {
            json!({"directs":[{"id":"direct-1","peer":{"user_id":PEER,"display_name":"Bea"}}]})
        } else if request.starts_with("GET /v1/members ") || request.starts_with("GET /v1/rooms ") {
            json!([])
        } else {
            return super::http(404, "Not Found", "{}");
        };
        super::http(200, "OK", &body.to_string())
    });
    let saved = Saved {
        base_url: base, token: "fixture-token".to_owned(), user_id: ME.to_owned(),
        email: None, display_name: "Ada".to_owned(), role: "member".to_owned(),
        plain_http_ok: false, e2ee: None,
    };
    let hub = std::sync::Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = protocol::AccountKey { network: protocol::Network::Tiny, account: ME.to_owned() };
    let (adapter, ctx, inbox) = crate::adapters::local(account, hub);
    super::start(ctx.clone(), &serde_json::to_string(&saved).map_err(|err| err.to_string())?, inbox)?;
    adapter.command(Command::Backfill("direct-1".to_owned()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Some(message) = ctx.stored_message("direct-1", "m-backfill").map_err(|err| err.to_string())? {
            return if message.text == "fetched after open" { Ok(()) } else { Err("backfill stored the wrong text".to_owned()) };
        }
        if std::time::Instant::now() >= deadline {
            return Err("opening the chat never fetched history".to_owned());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
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

/// One folded encrypted reaction, onto the stored message it landed on: the count and
/// ownership as this device folds everyone's blobs.
fn apply_reaction(ctx: &Context, conversation_id: &str, landed: &e2ee::ReactionLanded) {
    if !super::safe_id(&landed.message) || !unicode_emoji(&landed.emoji) {
        return;
    }
    let Ok(Some(mut message)) = ctx.stored_message(conversation_id, &landed.message) else {
        return;
    };
    let at = message.reactions.iter().position(|have| have.emoji == landed.emoji);
    if landed.count == 0 {
        if let Some(at) = at {
            message.reactions.remove(at);
        }
    } else if let Some(at) = at {
        message.reactions[at].count = landed.count;
        message.reactions[at].mine = landed.mine;
    } else {
        message.reactions.push(Reaction { emoji: landed.emoji.clone(), file: None, count: landed.count, mine: landed.mine });
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
            let opened = open_rows(ctx, saved, conversation_id, &wires.iter().collect::<Vec<_>>());
            let messages: Vec<Message> = wires.iter().filter(|wire| wire.kind.as_deref() != Some("edit")).filter_map(|wire| map_message(saved, &names, wire, &opened, &mut media)).collect();
            if !media.is_empty() {
                let _ = ctx.media(&media);
            }
            if !messages.is_empty() {
                ctx.history(conversation_id, &messages).map_err(|err| err.to_string())?;
            }
            // The reactions of an encrypted conversation are not rows of this page; the folded
            // ones that changed while this device was away are asked for now.
            if sealed_here(ctx, saved, conversation_id) == Ok(true) {
                if let Some(device) = e2ee::started(saved) {
                    for landed in device.lock().unwrap_or_else(|err| err.into_inner()).reaction_changes(conversation_id) {
                        apply_reaction(ctx, conversation_id, &landed);
                    }
                }
            }
            Ok(())
        }
        Ok((403 | 404, _)) => Ok(()),
        Ok((status, body)) => Err(super::explain(status, &body, super::Missing::Plain).to_owned()),
        Err(detail) => Err(detail.to_owned()),
    }
}

/// A send of text. It never runs on the commands thread: that one serves history, previews and
/// everything else the person is waiting on, and a send there waited behind all of it. A send
/// goes to this account's send thread, in order, and the commands go on meanwhile. One to an
/// encrypted conversation can wait for the people of a new group to join it (`e2ee::send`), so
/// it has a thread of its own, and does not hold up the plain ones.
fn send(ctx: &Context, saved: &Saved, conversation_id: String, client_id: String, text: String, reply_to: Option<String>) {
    if !super::safe_id(&conversation_id) {
        let outcome = Err(super::CHECK.to_owned());
        ctx.sent(&conversation_id, &client_id, outcome);
        return;
    }
    // The same question the post asks, so a conversation the device knows is encrypted but the
    // list has not labelled yet still goes to the sealed queue and never holds up the plain one.
    let account = send_account(saved, sealed_here(ctx, saved, &conversation_id) == Ok(true));
    let (posting, saved) = (ctx.clone(), saved.clone());
    in_order(account, Box::new(move || {
        let _ = crate::sendtime::run(&client_id, || {
            let outcome = post_message(&posting, &saved, &conversation_id, &client_id, &text, reply_to.as_deref(), &[]);
            let sent = outcome.is_ok();
            posting.sent(&conversation_id, &client_id, outcome);
            crate::sendtime::stage("store");
            if sent { Ok(()) } else { Err(()) }
        });
    }));
}

/// The send thread an account's sends of one kind go to: sealed ones apart from plain ones.
fn send_account(saved: &Saved, sealed: bool) -> (String, String) {
    let user = if sealed { saved.user_id.clone() } else { format!("{}#plain", saved.user_id) };
    (saved.base_url.clone(), user)
}

type Job = Box<dyn FnOnce() + Send>;

/// The send thread of each account (server and user, and plain or sealed), by what takes its jobs.
type Queues = HashMap<(String, String), std::sync::mpsc::Sender<Job>>;

/// Runs `job` on the account's send thread, after the ones before it. Made when first
/// needed, and again if it ended.
fn in_order(account: (String, String), job: Job) {
    static QUEUES: std::sync::OnceLock<std::sync::Mutex<Queues>> = std::sync::OnceLock::new();
    let mut queues = QUEUES.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let job = match queues.get(&account) {
        Some(queue) => match queue.send(job) {
            Ok(()) => return,
            Err(std::sync::mpsc::SendError(job)) => job,
        },
        None => job,
    };
    let (queue, jobs) = std::sync::mpsc::channel::<Job>();
    let started = std::thread::Builder::new().name("tiny-send".into()).spawn(move || {
        for job in jobs {
            job();
        }
    });
    match started {
        Ok(_) => {
            let _ = queue.send(job);
            queues.insert(account, queue);
        }
        // No thread to be had: this one sends it.
        Err(_) => {
            drop(queues);
            job();
        }
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
    let answer = super::asked("POST", saved, &path, Some(Value::Object(body)));
    crate::sendtime::stage("post");
    match answer {
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
    let labelled = ctx.known(conversation_id).ok().flatten().is_some_and(|row| row.encryption.is_sealed());
    let Some(device) = e2ee::started(saved) else {
        return if labelled { Err(NOT_SET_UP.to_owned()) } else { Ok(false) };
    };
    let mut device = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let encrypted = labelled || device.is_encrypted(conversation_id)?;
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
    let answer = e2ee::send(&device, conversation_id, client_id, e2ee_core::Kind::Message, None, &envelope, attachment_ids)?;
    accept_message(ctx, saved, conversation_id, &answer)
}

fn send_with(ctx: &Context, saved: &Saved, conversation_id: String, client_id: String, text: String, reply_to: Option<String>, outgoing: Outgoing) {
    if outgoing.files.is_empty() {
        send(ctx, saved, conversation_id, client_id, text, reply_to);
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

fn react(ctx: &Context, saved: &Saved, conversation_id: &str, message_id: &str, emoji: &str, on: bool, reply: crate::adapters::Reply<()>) {
    if !unicode_emoji(emoji) {
        let _ = reply.send(Err(Refusal::Refused("That reaction isn't allowed here.".to_owned())));
        return;
    }
    if !super::safe_id(message_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    match sealed_here(ctx, saved, conversation_id) {
        Ok(true) => {
            let result = e2ee::started(saved)
                .ok_or(NOT_SET_UP.to_owned())
                .and_then(|shared| {
                    let landed = shared
                        .lock()
                        .unwrap_or_else(|err| err.into_inner())
                        .react(conversation_id, message_id, emoji, on)?;
                    Ok((shared, landed))
                });
            match result {
                Ok((device, landed)) => {
                    drop(device);
                    if let Some(landed) = landed {
                        apply_reaction(ctx, conversation_id, &landed);
                    }
                    let _ = reply.send(Ok(()));
                }
                Err(why) => {
                    let _ = reply.send(Err(Refusal::Refused(why)));
                }
            }
            return;
        }
        Ok(false) => {}
        Err(why) => { let _ = reply.send(Err(Refusal::Refused(why))); return; }
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
                relink(&mut message);
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
    envelope.attachments = device.editable_attachments(conversation_id, message_id)?;
    let client_id = format!("edit-{message_id}-{}", crate::hub::now());
    device.post(conversation_id, &client_id, e2ee_core::Kind::Edit, Some(message_id), &envelope, &[])?;
    drop(device);
    if let Ok(Some(mut message)) = ctx.stored_message(conversation_id, message_id) {
        message.text = text.to_owned();
        message.edited = true;
        relink(&mut message);
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

fn start_direct(
    ctx: &Context,
    saved: &Saved,
    person_id: &str,
    reply: crate::adapters::Reply<String>,
) {
    start_direct_intent(ctx, saved, person_id, false, reply);
}

fn start_direct_intent(ctx: &Context, saved: &Saved, person_id: &str, call: bool, reply: crate::adapters::Reply<String>) {
    if !super::safe_id(person_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    // A new direct message is end-to-end encrypted when this device can be in one and the server
    // allows it; a server that requires it gets nothing else.
    let encrypted = if call {
        // A text device is deliberately not enrolled here. The server permits making an encrypted
        // conversation before either device exists; text sends still use their normal guard.
        let capabilities = match super::asked("GET", saved, "/v1/capabilities", None) {
            Ok((200, body)) => body,
            _ => {
                let _ = reply.send(Err(Refusal::Refused(super::UNREACHABLE.to_owned())));
                return;
            }
        };
        let features = capabilities.get("features").and_then(Value::as_array);
        let has = |feature: &str| features.is_some_and(|features| features.iter().any(|value| value.as_str() == Some(feature)));
        if !has(TINY_FEATURE_DIRECT_CALLS) {
            let _ = reply.send(Err(Refusal::Unsupported));
            return;
        }
        has(protocol::TINY_FEATURE_E2EE_TEXT)
            && capabilities.get("e2ee_text").and_then(Value::as_str).unwrap_or("optional") != "off"
    } else {
    let mode = e2ee::started(saved).map(|device| {
        let device = device
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (device.ready(), device.mode().to_owned())
    });
    match mode {
        Some((true, mode)) => mode != "off" && !mode.is_empty(),
        Some((false, mode)) if mode == "new_required" || mode == "required" => {
            let _ = reply.send(Err(Refusal::Refused(NOT_SET_UP.to_owned())));
            return;
        }
        _ => false,
    } };
    match super::asked(
        "POST",
        saved,
        "/v1/directs",
        Some(json!({ "user_id": person_id, "e2ee": encrypted })),
    ) {
        Ok((200 | 201, body)) => {
            let id = body
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| body.pointer("/direct/id").and_then(Value::as_str))
                .unwrap_or("");
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
                row.call = if directs_callable(saved) { CallSupport::Native } else { CallSupport::None };
                row.encryption = encryption_of(saved, id, encrypted, false);
                let _ = ctx.conversation(&row);
                let _ = ctx.counts(id, 0, 0, None, 1);
            }
            let _ = reply.send(Ok(id.to_owned()));
        }
        Ok((status, body)) => {
            let _ = reply.send(Err(Refusal::Refused(
                super::explain(status, &body, super::Missing::Plain).to_owned(),
            )));
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
    let opened = open_rows(ctx, saved, conversation, &[&wire]);
    crate::sendtime::stage("open");
    // Our own message is named from the session. The people list (a full request to the server)
    // is for the rest, and nothing waits on it to settle a row of ours.
    let own = wire.author_id == saved.user_id && !saved.display_name.is_empty();
    let names = if own { HashMap::new() } else { member_names(saved) };
    if !own {
        crate::sendtime::stage("members");
    }
    let message = map_message(saved, &names, &wire, &opened, &mut media).ok_or(super::UNREADABLE)?;
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
    if row_is_sealed(wire) {
        return map_sealed(saved, names, wire, opened.get(&wire.id), deleted, sent, outgoing, media);
    }
    let mut message = Message::plain(&wire.id, name_of(names, saved, &wire.author_id), if deleted { "" } else { &wire.body }, sent, outgoing);
    message.sender.id = wire.author_id.clone();
    message.edited = present(&wire.edited_at);
    message.deleted = deleted;
    message.client_id = wire.client_id.clone().filter(|id| !id.is_empty());
    message.seq = wire.seq;
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
    relink(&mut message);
    Some(message)
}

/// Web addresses in the text become links. The spans point into the text as it is now, so those
/// from before an edit go; Tiny sends plain text and has no other styling.
fn relink(message: &mut Message) {
    message.spans.clear();
    protocol::link::linkify(message);
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
    message.seq = wire.seq;
    message.seal = Seal::Encrypted;
    if !deleted {
        match read {
            Some(Read::Opened(opened)) | Some(Read::Legacy(opened)) => {
                if let Some(auth) = &opened.auth {
                    let author = auth.author.to_string();
                    message.sender.id = author.clone();
                    message.sender.name = name_of(names, saved, &author);
                    message.outgoing = author == saved.user_id;
                } else {
                    // Unauthenticated cache hits (device-link archive, pre-auth caches, decrypt
                    // fallback) keep the wire author for display only. Clearing id made every
                    // speaker share one empty id, so continues() collapsed a back-and-forth into
                    // one monologue. saved_history still blocks edits; MLS auth still wins above.
                    message.saved_history = true;
                    message.client_id = None;
                    if let Some(attribution) = &opened.saved_attribution {
                        message.sender.id = attribution.id.clone();
                        message.sender.name = attribution.name.clone();
                        message.outgoing = attribution.outgoing;
                        message.sent = attribution.sent;
                        message.sent_subsec_millis = attribution.sent_subsec_millis;
                    } else if message.sent == 0 && opened.at != 0 {
                        message.sent = opened.at;
                        message.sent_subsec_millis = 0;
                    }
                }
                if let Some(millis) = opened.sent_at {
                    message.sent = millis.div_euclid(1000);
                    message.sent_subsec_millis = millis.rem_euclid(1000) as u32;
                }
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
    relink(&mut message);
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

/// Unicode emoji only. A shortcode, a word, or a path is not one. A keycap is one, though it
/// starts with an ASCII digit (1️⃣ is `1`, U+FE0F, U+20E3), and tinyserver takes it.
fn unicode_emoji(emoji: &str) -> bool {
    let mut textual = false;
    if emoji.is_empty() {
        return false;
    }
    for (at, ch) in emoji.char_indices() {
        let after = &emoji[at + ch.len_utf8()..];
        let keycap = ch.is_ascii_digit() && (after.starts_with('\u{20E3}') || after.starts_with("\u{FE0F}\u{20E3}"));
        if !keycap && (ch.is_ascii_alphanumeric() || ch == ':' || ch == '/') {
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
        direct_calls: directs_callable(saved),
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

/// Whether the stored space already has text channels, directs, and/or voice rooms. Used to
/// refuse an empty list that would delete those rows and their messages. Directs sort after
/// channels, so this must not use a paged tree read.
fn kept_list_kinds(ctx: &Context) -> (bool, bool, bool) {
    ctx.hub.store().space_text_kinds(&ctx.key(SPACE)).unwrap_or((false, false, false))
}

/// The ids of the conversations the person is in: their channels and their direct messages.
/// `None` when either list could not be read.
pub(crate) fn conversation_ids(saved: &Saved) -> Option<std::collections::BTreeSet<String>> {
    let channels = optional_list::<WireChannel>(saved, "/v1/channels", "channels")?;
    let directs = optional_list::<WireDirect>(saved, "/v1/directs", "directs")?;
    Some(channels.into_iter().map(|channel| channel.id).chain(directs.into_iter().map(|direct| direct.id)).collect())
}

fn optional_list<T: serde::de::DeserializeOwned>(saved: &Saved, path: &str, key: &str) -> Option<Vec<T>> {
    match super::asked("GET", saved, path, None) {
        // A refusal is not an empty server. An empty list deletes every conversation left out,
        // and the messages stored with them.
        Ok((status @ (403 | 404), _)) => {
            tracing::warn!("chatd: Tiny server: {path} answered {status}");
            None
        }
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
    /// End-to-end encrypted, for good once it is. Absent on a server older than encrypted text.
    #[serde(default)]
    e2ee: bool,
    /// Encrypted now, and some message from before that is still stored in the clear.
    #[serde(default)]
    plaintext_history: bool,
}

#[derive(Clone, Deserialize)]
struct WireDirect {
    id: String,
    peer: WirePeer,
    #[serde(default)]
    e2ee: bool,
    /// Encrypted now, and some message from before that is still stored in the clear.
    #[serde(default)]
    plaintext_history: bool,
    #[serde(default, deserialize_with = "flex_u32")]
    unread: u32,
    #[serde(default)]
    last_read: Option<String>,
    #[serde(default)]
    peer_read: Option<String>,
    #[serde(default)]
    last_message: Option<WireLast>,
    /// The newest finished call with this person. Absent until one has ended.
    #[serde(default)]
    last_call: Option<WireCall>,
}

/// One finished direct call. `GET /v1/directs` names it `id`; a live `direct.call` names it `ring_id`.
#[derive(Clone, Deserialize, Default)]
struct WireCall {
    #[serde(default)]
    id: String,
    #[serde(default)]
    ring_id: String,
    #[serde(default)]
    direct_id: String,
    #[serde(default)]
    caller_id: String,
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    ended_at: String,
}

/// The grey line for a finished call. Nothing about the conversation row changes except the
/// unread count, and only for a missed call we did not place.
fn record_call(
    ctx: &Context,
    saved: &Saved,
    names: &HashMap<String, String>,
    direct_id: &str,
    call: &WireCall,
) {
    let ring_id = if call.ring_id.is_empty() {
        call.id.as_str()
    } else {
        call.ring_id.as_str()
    };
    if !super::safe_id(direct_id) || !super::safe_id(ring_id) || !super::safe_id(&call.caller_id) {
        return;
    }
    if !matches!(
        call.outcome.as_str(),
        "missed" | "declined" | "cancelled" | "ended"
    ) {
        return;
    }
    if ctx.known(direct_id).ok().flatten().is_none() {
        return;
    }
    let outgoing = call.caller_id == saved.user_id;
    let name = caller_name(ctx, saved, names, direct_id, &call.caller_id);
    let mut message = Message::plain(
        ring_id,
        name.clone(),
        format!("{name} started a call."),
        rfc3339(&call.ended_at).unwrap_or(0),
        outgoing,
    );
    message.kind = MessageKind::System;
    message.sender.id = call.caller_id.clone();
    let _ = ctx.quiet_line(direct_id, &message, call.outcome == "missed" && !outgoing);
}

fn caller_name(
    ctx: &Context,
    saved: &Saved,
    names: &HashMap<String, String>,
    direct_id: &str,
    caller_id: &str,
) -> String {
    if caller_id == saved.user_id {
        return if saved.display_name.is_empty() {
            "Someone".to_owned()
        } else {
            saved.display_name.clone()
        };
    }
    if let Some(name) = names.get(caller_id).filter(|name| !name.is_empty()) {
        return name.clone();
    }
    if let Ok(Some(row)) = ctx.known(direct_id) {
        if row.peer.as_deref() == Some(caller_id) && !row.title.is_empty() {
            return row.title;
        }
    }
    "Someone".to_owned()
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
    #[serde(default, deserialize_with = "message_generation")]
    generation: Option<u64>,
    #[serde(default, deserialize_with = "message_group")]
    group_id: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    target_id: Option<String>,
    /// Post order. Set on every message a current server sends. Absent on an older one,
    /// which leaves the clock and the id to break a tie.
    #[serde(default)]
    seq: i64,
}

fn message_generation<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    let generation = u64::deserialize(deserializer)?;
    if generation > i64::MAX as u64 { return Err(serde::de::Error::custom("invalid encryption generation")); }
    Ok(Some(generation))
}

fn message_group<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let group = String::deserialize(deserializer)?;
    e2ee_core::ConversationId::parse(&group).map_err(serde::de::Error::custom)?;
    Ok(Some(group))
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

/// Proves unauthenticated sealed cache rows keep distinct wire speakers instead of collapsing to
/// one empty-id "Saved message". The lab sets `TINYCLIENT_PROVE=legacy-saved-speakers`.
pub(super) fn prove_legacy_saved_speakers() -> Result<(), String> {
    const ME: &str = "11111111-1111-1111-1111-111111111111";
    const PEER: &str = "22222222-2222-2222-2222-222222222222";
    let saved = Saved {
        base_url: "http://127.0.0.1:9".to_owned(),
        token: "t".to_owned(),
        user_id: ME.to_owned(),
        email: None,
        display_name: "Justin".to_owned(),
        role: "member".to_owned(),
        plain_http_ok: false,
        e2ee: None,
    };
    let names = HashMap::from([(PEER.to_owned(), "Larsen".to_owned())]);
    let mine: WireMessage = serde_json::from_value(serde_json::json!({
        "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "author_id": ME, "e2ee": true,
        "created_at": "2024-01-02T03:04:05Z"
    }))
    .map_err(|err| err.to_string())?;
    let theirs: WireMessage = serde_json::from_value(serde_json::json!({
        "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "author_id": PEER, "e2ee": true,
        "created_at": "2024-01-02T03:04:06Z"
    }))
    .map_err(|err| err.to_string())?;
    let opened = e2ee::Opened {
        text: "hi".into(),
        ..e2ee::Opened::default()
    };
    let reads = HashMap::from([
        (mine.id.clone(), Read::Legacy(opened.clone())),
        (theirs.id.clone(), Read::Legacy(opened)),
    ]);
    let mine = map_message(&saved, &names, &mine, &reads, &mut Vec::new()).ok_or("mine missing")?;
    let theirs = map_message(&saved, &names, &theirs, &reads, &mut Vec::new()).ok_or("theirs missing")?;
    if !mine.saved_history || !theirs.saved_history {
        return Err("legacy rows must stay marked as saved history".into());
    }
    if mine.sender.id != ME || !mine.outgoing || mine.sender.name != "Justin" {
        return Err(format!("mine mapped as {} / {} outgoing={}", mine.sender.id, mine.sender.name, mine.outgoing));
    }
    if theirs.sender.id != PEER || theirs.outgoing || theirs.sender.name != "Larsen" {
        return Err(format!("theirs mapped as {} / {} outgoing={}", theirs.sender.id, theirs.sender.name, theirs.outgoing));
    }
    if mine.sender.id == theirs.sender.id {
        return Err("both speakers collapsed to the same id".into());
    }
    Ok(())
}

/// Proves a Tiny message's web addresses are links, in a plaintext row and in an opened encrypted
/// one, with the spans on the right bytes after multibyte text and the address left as typed.
/// The lab sets `TINYCLIENT_PROVE=linkify`. A build from before the feature ignores that and keeps
/// serving, so the same driver fails there.
pub(super) fn prove_linkify() -> Result<(), String> {
    const ME: &str = "11111111-1111-1111-1111-111111111111";
    let saved = Saved {
        base_url: "http://127.0.0.1:9".to_owned(),
        token: "t".to_owned(),
        user_id: ME.to_owned(),
        email: None,
        display_name: "Ada".to_owned(),
        role: "member".to_owned(),
        plain_http_ok: false,
        e2ee: None,
    };
    let text = "héllo 你好 docs: https://example.com/a%20b?q=1&r=2#top, and (www.example.org/x). Not mailto:a@example.com or `https://example.com/code`";
    let want = ["https://example.com/a%20b?q=1&r=2#top", "www.example.org/x"];
    let plain: WireMessage = serde_json::from_value(serde_json::json!({
        "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "author_id": ME, "body": text, "created_at": "2024-01-02T03:04:05Z"
    }))
    .map_err(|err| err.to_string())?;
    let sealed: WireMessage = serde_json::from_value(serde_json::json!({
        "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "author_id": ME, "e2ee": true, "created_at": "2024-01-02T03:04:06Z"
    }))
    .map_err(|err| err.to_string())?;
    let opened = e2ee::Opened { text: text.into(), ..e2ee::Opened::default() };
    let reads = HashMap::from([(sealed.id.clone(), Read::Legacy(opened))]);
    for (what, wire) in [("plaintext", &plain), ("encrypted", &sealed)] {
        let message = map_message(&saved, &HashMap::new(), wire, &reads, &mut Vec::new()).ok_or(format!("the {what} row was dropped"))?;
        let linked: Vec<(String, String)> = message
            .spans
            .iter()
            .filter_map(|span| match &span.style {
                protocol::Style::Link { url } => message.text.get(span.start as usize..span.end as usize).map(|shown| (shown.to_owned(), url.clone())),
                _ => None,
            })
            .collect();
        let got: Vec<&str> = linked.iter().map(|(shown, _)| shown.as_str()).collect();
        if got != want {
            return Err(format!("the {what} row linked {got:?}, not {want:?}"));
        }
        if linked[1].1 != "https://www.example.org/x" || linked[0].1 != want[0] {
            return Err(format!("the {what} row opens {:?}", linked.iter().map(|(_, url)| url).collect::<Vec<_>>()));
        }
        if message.text != text {
            return Err(format!("the {what} row's text was changed"));
        }
    }
    Ok(())
}

/// Proves a ciphertext row with no `e2ee` flag is stored as undecryptable, not as blank plaintext.
/// The lab sets `TINYCLIENT_PROVE=sealed-row`. A build from before the fix ignores that and keeps
/// serving, so the same driver fails there.
pub(super) fn prove_missing_seal() -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let base = format!("http://{}", listener.local_addr().map_err(|err| err.to_string())?);
    std::thread::spawn(move || answer_seal_probe(listener));
    let saved = Saved {
        base_url: base,
        token: "t".to_owned(),
        user_id: "11111111-1111-1111-1111-111111111111".to_owned(),
        email: None,
        display_name: "Ada".to_owned(),
        role: "owner".to_owned(),
        plain_http_ok: false,
        e2ee: None,
    };
    let hub = std::sync::Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = protocol::AccountKey { network: protocol::Network::Tiny, account: saved.user_id.clone() };
    let (_adapter, ctx, _inbox) = crate::adapters::local(account, hub);
    load_page(&ctx, &saved, "direct-1", None, PAGE);
    let stored = ctx.stored_message("direct-1", "s-2").map_err(|err| err.to_string())?.ok_or("the ciphertext row was not stored")?;
    if stored.seal == Seal::Undecryptable && stored.text == protocol::UNDECRYPTABLE {
        Ok(())
    } else {
        Err(format!("the row was stored as {:?} with text {:?}", stored.seal, stored.text))
    }
}

/// Proves an empty channel and direct list does not delete messages already stored.
/// The lab sets `TINYCLIENT_PROVE=empty-list`. A build from before the fix ignores that and
/// keeps serving, so the same driver fails there.
pub(super) fn prove_empty_list() -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let base = format!("http://{}", listener.local_addr().map_err(|err| err.to_string())?);
    std::thread::spawn(move || answer_empty_list(listener));
    let saved = Saved {
        base_url: base,
        token: "t".to_owned(),
        user_id: "11111111-1111-1111-1111-111111111111".to_owned(),
        email: None,
        display_name: "Ada".to_owned(),
        role: "owner".to_owned(),
        plain_http_ok: false,
        e2ee: None,
    };
    let hub = std::sync::Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = protocol::AccountKey { network: protocol::Network::Tiny, account: saved.user_id.clone() };
    let (_adapter, ctx, _inbox) = crate::adapters::local(account, hub);
    refresh(&ctx, &saved);
    if ctx.known("channel-1").map_err(|err| err.to_string())?.is_none() {
        return Err("the channel list was not stored".to_owned());
    }
    let kept = Message::plain("m-kept", "Ada", "kept", 1_700_000_000, false);
    ctx.hub.store().ingest(&ctx.key("channel-1"), &kept, false).map_err(|err| err.to_string())?;
    refresh(&ctx, &saved);
    match ctx.stored_message("channel-1", "m-kept").map_err(|err| err.to_string())? {
        Some(message) if message.text == "kept" => Ok(()),
        Some(message) => Err(format!("the stored text was {:?}", message.text)),
        None => Err("an empty channel list deleted the stored message".to_owned()),
    }
}

fn answer_empty_list(listener: std::net::TcpListener) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let channels = AtomicUsize::new(0);
    let _ = listener.set_nonblocking(true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let listed = r#"{"channels":[{"id":"channel-1","name":"general"}]}"#;
    while std::time::Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut got = [0u8; 2048];
                let read = std::io::Read::read(&mut stream, &mut got).unwrap_or(0);
                let request = String::from_utf8_lossy(&got[..read]);
                let (status, payload) = if request.contains("GET /v1/channels") {
                    if channels.fetch_add(1, Ordering::Relaxed) == 0 {
                        ("200 OK", listed)
                    } else {
                        ("200 OK", r#"{"channels":[]}"#)
                    }
                } else if request.contains("GET /v1/directs") {
                    ("200 OK", r#"{"directs":[]}"#)
                } else if request.contains("GET /v1/rooms") || request.contains("GET /v1/members") {
                    ("200 OK", "[]")
                } else {
                    ("404 Not Found", "{}")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(_) => break,
        }
    }
}

/// Proves a refused channel list does not delete messages already stored.
/// The lab sets `TINYCLIENT_PROVE=refused-list`. A build from before the fix ignores that and
/// keeps serving, so the same driver fails there.
pub(super) fn prove_refused_list() -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let base = format!("http://{}", listener.local_addr().map_err(|err| err.to_string())?);
    std::thread::spawn(move || answer_refused_list(listener));
    let saved = Saved {
        base_url: base,
        token: "t".to_owned(),
        user_id: "11111111-1111-1111-1111-111111111111".to_owned(),
        email: None,
        display_name: "Ada".to_owned(),
        role: "owner".to_owned(),
        plain_http_ok: false,
        e2ee: None,
    };
    let hub = std::sync::Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = protocol::AccountKey { network: protocol::Network::Tiny, account: saved.user_id.clone() };
    let (_adapter, ctx, _inbox) = crate::adapters::local(account, hub);
    refresh(&ctx, &saved);
    if ctx.known("channel-1").map_err(|err| err.to_string())?.is_none() {
        return Err("the channel list was not stored".to_owned());
    }
    let kept = Message::plain("m-kept", "Ada", "kept", 1_700_000_000, false);
    ctx.hub.store().ingest(&ctx.key("channel-1"), &kept, false).map_err(|err| err.to_string())?;
    refresh(&ctx, &saved);
    match ctx.stored_message("channel-1", "m-kept").map_err(|err| err.to_string())? {
        Some(message) if message.text == "kept" => Ok(()),
        Some(message) => Err(format!("the stored text was {:?}", message.text)),
        None => Err("a refused channel list deleted the stored message".to_owned()),
    }
}

fn answer_refused_list(listener: std::net::TcpListener) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let channels = AtomicUsize::new(0);
    let _ = listener.set_nonblocking(true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let listed = r#"{"channels":[{"id":"channel-1","name":"general"}]}"#;
    while std::time::Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut got = [0u8; 2048];
                let read = std::io::Read::read(&mut stream, &mut got).unwrap_or(0);
                let request = String::from_utf8_lossy(&got[..read]);
                let (status, payload) = if request.contains("GET /v1/channels") {
                    if channels.fetch_add(1, Ordering::Relaxed) == 0 {
                        ("200 OK", listed)
                    } else {
                        ("403 Forbidden", r#"{"error":"forbidden"}"#)
                    }
                } else if request.contains("GET /v1/directs") {
                    ("200 OK", r#"{"directs":[]}"#)
                } else if request.contains("GET /v1/rooms") || request.contains("GET /v1/members") {
                    ("200 OK", "[]")
                } else {
                    ("404 Not Found", "{}")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(_) => break,
        }
    }
}

fn answer_seal_probe(listener: std::net::TcpListener) {
    let _ = listener.set_nonblocking(true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let sealed = r#"{"messages":[{"id":"s-2","author_id":"22222222-2222-2222-2222-222222222222","body":"","ciphertext":"AAAA","epoch":3,"device_id":"d-1","created_at":"2024-01-02T03:04:05Z"}]}"#;
    while std::time::Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut got = [0u8; 2048];
                let read = std::io::Read::read(&mut stream, &mut got).unwrap_or(0);
                let request = String::from_utf8_lossy(&got[..read]);
                let (status, payload) = if request.contains("/messages") { ("200 OK", sealed) } else { ("404 Not Found", "{}") };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(_) => break,
        }
    }
}

fn answer_keep_opened_probe(listener: std::net::TcpListener) {
    let _ = listener.set_nonblocking(true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let sealed = r#"{"messages":[{"id":"s-keep","author_id":"22222222-2222-2222-2222-222222222222","body":"","e2ee":true,"ciphertext":"AAAA","epoch":3,"device_id":"d-1","created_at":"2024-01-02T03:04:05Z"}]}"#;
    while std::time::Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut got = [0u8; 2048];
                let read = std::io::Read::read(&mut stream, &mut got).unwrap_or(0);
                let request = String::from_utf8_lossy(&got[..read]);
                let (status, payload) = if request.contains("/messages") {
                    ("200 OK", sealed)
                } else {
                    ("404 Not Found", "{}")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            Err(_) => break,
        }
    }
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
    fn call_intent_creates_an_encrypted_direct_without_a_text_device() {
        let posted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = posted.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/capabilities ") {
                return Some(http(200, "OK", r#"{"features":["direct_calls","e2ee_text"],"e2ee_text":"required"}"#));
            }
            if request.starts_with("POST /v1/directs ") {
                seen.lock().unwrap().push(request.to_owned());
                return Some(http(201, "Created", r#"{"id":"first-call","e2ee":true}"#));
            }
            None
        });
        assert!(e2ee::started(&saved).is_none(), "no text enrollment");
        assert_eq!(serde_json::to_value(account_caps(&saved)).unwrap()["direct_calls"], true, "members can call before a direct row exists");
        let (reply, result) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &Live::new().unwrap(), Command::StartDirectCall { person_id: PEER.into(), reply });
        assert_eq!(result.recv().unwrap().unwrap(), "first-call");
        let row = ctx.known("first-call").unwrap().unwrap();
        assert_eq!(row.call, CallSupport::Native);
        assert_eq!(row.encryption, Encryption::EndToEnd);
        let requests = posted.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["e2ee"], true, "call setup never creates plaintext text history");
        assert_eq!(post_message(&ctx, &saved, "first-call", "client-call", "secret", None, &[]).unwrap_err(), NOT_SET_UP, "text enrollment is still required");
    }

    #[test]
    fn direct_calls_capability_applies_to_list_created_and_fallback_directs() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/capabilities ") {
                return Some(http(200, "OK", r#"{"features":["direct_calls"]}"#));
            }
            if request.starts_with("POST /v1/directs ") {
                return Some(http(201, "Created", r#"{"id":"direct-new"}"#));
            }
            None
        });
        assert_eq!(ctx.known("direct-1").unwrap().unwrap().call, CallSupport::Native, "listed direct is callable");
        on_live(&ctx, &saved, &HashMap::new(), "direct.created", &json!({
            "direct": {"id":"direct-live","peer":{"user_id":PEER,"display_name":"Bea"}}
        }));
        assert_eq!(ctx.known("direct-live").unwrap().unwrap().call, CallSupport::Native, "new incoming direct is callable");
        let (reply, result) = std::sync::mpsc::channel();
        start_direct(&ctx, &saved, PEER, reply);
        assert_eq!(result.recv().unwrap().unwrap(), "direct-new");
        assert_eq!(ctx.known("direct-new").unwrap().unwrap().call, CallSupport::Native, "first call needs no prior messages");
    }

    #[test]
    fn an_older_server_does_not_offer_direct_calls() {
        let (_adapter, ctx, _saved) = fixture(|_| None);
        assert_eq!(ctx.known("direct-1").unwrap().unwrap().call, CallSupport::None);
    }

    #[test]
    fn encrypted_history_uses_private_sender_time_and_preserves_subsecond_order() {
        let (_adapter, ctx) = open_ctx();
        let saved = saved("http://127.0.0.1:9");
        ctx.conversation(&ConversationRow::new("sealed", ConversationKind::Channel, "Secret")).unwrap();
        let mut mapped = Vec::new();
        for (id, millis) in [("z-first", 1_704_164_645_100_i64), ("a-second", 1_704_164_645_900)] {
            let wire: WireMessage = serde_json::from_value(json!({
                "id": id, "author_id": PEER, "e2ee": true,
                "created_at": "2024-01-02T03:04:00Z"
            })).unwrap();
            // This is also the representation restored from a linked device's archive.
            let opened = serde_json::from_value(json!({"text": id, "sent_at": millis})).unwrap();
            let reads = HashMap::from([(id.to_owned(), Read::Opened(opened))]);
            mapped.push(map_message(&saved, &HashMap::new(), &wire, &reads, &mut Vec::new()).unwrap());
        }
        assert_eq!(mapped[0].sent, 1_704_164_645, "the minute-rounded server date is only a fallback");
        ctx.history("sealed", &mapped).unwrap();
        let page = ctx.hub.store().messages(&ctx.key("sealed"), &Page::Newest, 10, 1_800_000_000).unwrap().unwrap();
        assert_eq!(page.messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["z-first", "a-second"]);
        let legacy: WireMessage = serde_json::from_value(json!({
            "id": "legacy", "author_id": PEER, "e2ee": true, "created_at": "2024-01-02T03:04:00Z"
        })).unwrap();
        let reads = HashMap::from([("legacy".to_owned(), Read::Opened(e2ee::Opened::default()))]);
        let legacy = map_message(&saved, &HashMap::new(), &legacy, &reads, &mut Vec::new()).unwrap();
        assert_eq!((legacy.sent, legacy.sent_subsec_millis), (1_704_164_640, 0), "older envelopes and caches retain their server date");
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

    /// A keycap reaction (1️⃣) was refused when sent and dropped when another app had put one on
    /// a message, because it starts with an ASCII digit.
    #[test]
    fn a_keycap_is_a_reaction_and_a_bare_digit_is_not() {
        assert!(unicode_emoji("1\u{FE0F}\u{20E3}"));
        assert!(unicode_emoji("7\u{20E3}"), "a keycap without its variation selector");
        assert!(unicode_emoji("*\u{FE0F}\u{20E3}"));
        assert!(!unicode_emoji("1"));
        assert!(!unicode_emoji("1\u{FE0F}"), "a digit and a variation selector are not a keycap");
        assert!(!unicode_emoji("10\u{FE0F}\u{20E3}"), "only the digit right before the keycap mark is part of it");
        assert!(!unicode_emoji("a\u{20E3}"));
        assert!(super::prove_keycap_reaction().is_ok());
    }

    #[test]
    fn a_new_direct_event_records_the_peer_on_either_side_before_the_first_message() {
        for (low, high) in [(ME, PEER), (PEER, ME)] {
            let (_adapter, ctx) = open_ctx();
            let saved = saved("http://127.0.0.1:1");
            let names = HashMap::from([(PEER.to_owned(), "Bea".to_owned())]);
            let frame = json!({"direct": {"id": "new-direct", "low_user_id": low,
                "high_user_id": high, "e2ee": true, "created_at": WHEN}});
            on_live(&ctx, &saved, &names, "direct.created", &frame);
            let row = ctx.known("new-direct").unwrap().expect("new direct is recorded before a message arrives");
            assert_eq!((row.title.as_str(), row.peer.as_deref(), row.kind, row.encryption),
                ("Bea", Some(PEER), ConversationKind::Direct, Encryption::EndToEnd));
            assert_eq!(row.group_title, "Direct messages");
            assert_eq!(ctx.hub.store().person(&ctx.key(PEER)).unwrap().unwrap().name, "Bea");

            let first = json!({"conversation_id": "new-direct", "message": {
                "id": "first", "author_id": PEER, "body": "hello Ada", "created_at": WHEN}});
            on_live(&ctx, &saved, &names, "message.created", &first);
            // Replayed creation must not clear the first message's preview or unread count.
            ctx.counts("new-direct", 1, 0, Some("read-before"), rfc3339(WHEN).unwrap()).unwrap();
            ctx.mark_encryption("new-direct", Encryption::EndToEndAfterPlain).unwrap();
            on_live(&ctx, &saved, &HashMap::new(), "direct.created", &frame);
            on_live(&ctx, &saved, &names, "message.created", &first);
            let (rows, total) = ctx.hub.store().inbox(Some(Network::Tiny), InboxFilter::Directs, 0, 10, &[]).unwrap();
            assert_eq!(total, 1);
            assert_eq!(rows.len(), 1);
            assert_eq!((rows[0].title.as_str(), rows[0].preview.as_str(), rows[0].unread), ("Bea", "hello Ada", 1));
            assert_eq!(rows[0].encryption, Encryption::EndToEndAfterPlain);
            assert_eq!(rows[0].last_read.as_deref(), Some("read-before"));
        }
    }

    #[test]
    fn a_stale_direct_creation_cannot_downgrade_an_encrypted_conversation() {
        for encryption in [Encryption::EndToEnd, Encryption::EndToEndAfterPlain] {
            for old_flag in [Some(false), None] {
                let (_adapter, ctx) = open_ctx();
                let saved = saved("http://127.0.0.1:1");
                let mut row = ConversationRow::new("upgraded-direct", ConversationKind::Direct, "Bea");
                row.encryption = encryption;
                ctx.hub.store().upsert_conversation(&ctx.account, &row).unwrap();
                let mut direct = json!({"id": "upgraded-direct", "low_user_id": ME, "high_user_id": PEER});
                if let Some(flag) = old_flag { direct["e2ee"] = json!(flag); }
                on_live(&ctx, &saved, &HashMap::new(), "direct.created", &json!({"direct": direct}));
                assert_eq!(ctx.known("upgraded-direct").unwrap().unwrap().encryption, encryption,
                    "stale e2ee={old_flag:?} must retain {encryption:?}");
                assert_ne!(sealed_here(&ctx, &saved, "upgraded-direct"), Ok(false),
                    "a creation replay must not permit plaintext sending");
            }
        }
    }

    #[test]
    fn sealed_and_plain_sends_go_to_different_send_threads() {
        let saved = saved("http://127.0.0.1:1");
        let (sealed, plain) = (send_account(&saved, true), send_account(&saved, false));
        assert_ne!(sealed, plain, "a sealed send that waits to join must not hold up a plain one");
        assert_eq!(sealed.0, plain.0, "the same server");
        assert_eq!(sealed.1, saved.user_id);
    }

    #[test]
    fn an_unrelated_or_invalid_direct_pair_is_not_published() {
        for (low, high) in [(PEER, "33333333"), (ME, ME), (ME, "bad/id"), ("", ME)] {
            let (_adapter, ctx) = open_ctx();
            let saved = saved("http://127.0.0.1:1");
            // A peer field must not bypass validation when a pair is present.
            let frame = json!({"direct": {"id": "new-direct", "low_user_id": low,
                "high_user_id": high, "peer": {"user_id": PEER, "display_name": "Bea"}}});
            on_live(&ctx, &saved, &HashMap::new(), "direct.created", &frame);
            assert!(ctx.known("new-direct").unwrap().is_none(), "invalid pair {low}/{high}");
        }
        for pair in [json!({"low_user_id": ME}), json!({"high_user_id": ME}), json!({"low_user_id": ME, "high_user_id": null})] {
            let (_adapter, ctx) = open_ctx();
            let saved = saved("http://127.0.0.1:1");
            let mut direct = pair;
            direct["id"] = json!("incomplete-direct");
            direct["peer"] = json!({"user_id": PEER, "display_name": "Bea"});
            on_live(&ctx, &saved, &HashMap::new(), "direct.created", &json!({"direct": direct}));
            assert!(ctx.known("incomplete-direct").unwrap().is_none(), "incomplete pair rejected");
        }
    }

    #[test]
    fn a_direct_event_still_accepts_a_rest_shaped_peer() {
        let (_adapter, ctx) = open_ctx();
        let saved = saved("http://127.0.0.1:1");
        let frame = json!({"direct": {"id": "new-direct", "peer": {"user_id": PEER, "display_name": "Bea"},
            "e2ee": true, "unread": 2, "last_message": {"id": "last", "body": "hello", "created_at": WHEN}}});
        on_live(&ctx, &saved, &HashMap::new(), "direct.created", &frame);
        let row = ctx.known("new-direct").unwrap().unwrap();
        assert_eq!((row.title.as_str(), row.peer.as_deref(), row.encryption, row.unread),
            ("Bea", Some(PEER), Encryption::EndToEnd, 2));
    }

    #[test]
    fn web_addresses_in_a_message_become_links_and_an_edit_moves_them() {
        assert_eq!(super::prove_linkify(), Ok(()));
        let mut message = Message::plain("m", "Ada", "see https://a.example/x", 1, false);
        super::relink(&mut message);
        assert_eq!(message.spans.len(), 1);
        message.text = "now a much longer line before https://b.example/y".to_owned();
        super::relink(&mut message);
        let [span] = &message.spans[..] else { panic!("{:?}", message.spans) };
        assert_eq!(&message.text[span.start as usize..span.end as usize], "https://b.example/y");
        message.text = "no address left".to_owned();
        super::relink(&mut message);
        assert!(message.spans.is_empty());
    }

    #[test]
    fn legacy_saved_text_keeps_wire_display_attribution_and_marks_saved_history() {
        let mut saved = saved("http://unused");
        saved.display_name = "Justin".into();
        let wire: WireMessage = serde_json::from_value(json!({"id":"legacy", "author_id":ME, "e2ee":true, "created_at":WHEN, "client_id":"forged-own"})).unwrap();
        let opened = e2ee::Opened { text:"saved words".into(), at:123, ..e2ee::Opened::default() };
        let reads = HashMap::from([("legacy".to_owned(), Read::Legacy(opened))]);
        let mapped = map_message(&saved, &HashMap::new(), &wire, &reads, &mut Vec::new()).unwrap();
        assert_eq!(mapped.text, "saved words");
        assert_eq!(mapped.sender.name, "Justin");
        assert_eq!(mapped.sender.id, ME);
        assert!(mapped.outgoing);
        assert!(mapped.client_id.is_none());
        assert!(mapped.saved_history);
        // Wire created_at wins over coarse opened.at so speakers keep distinct timestamps.
        assert_eq!(mapped.sent, rfc3339(WHEN).unwrap());
    }

    #[test]
    fn legacy_saved_text_from_two_wire_authors_keeps_both_speakers() {
        let saved = saved("http://unused");
        let names = HashMap::from([(PEER.to_owned(), "Larsen".to_owned())]);
        let mine: WireMessage = serde_json::from_value(json!({"id":"mine", "author_id":ME, "e2ee":true, "created_at":WHEN})).unwrap();
        let theirs: WireMessage = serde_json::from_value(json!({"id":"theirs", "author_id":PEER, "e2ee":true, "created_at":WHEN})).unwrap();
        let opened = e2ee::Opened { text:"hi".into(), ..e2ee::Opened::default() };
        let reads = HashMap::from([
            ("mine".to_owned(), Read::Legacy(opened.clone())),
            ("theirs".to_owned(), Read::Legacy(opened)),
        ]);
        let mine = map_message(&saved, &names, &mine, &reads, &mut Vec::new()).unwrap();
        let theirs = map_message(&saved, &names, &theirs, &reads, &mut Vec::new()).unwrap();
        assert_eq!((mine.sender.id.as_str(), mine.outgoing, mine.saved_history), (ME, true, true));
        assert_eq!((theirs.sender.id.as_str(), theirs.sender.name.as_str(), theirs.outgoing, theirs.saved_history),
            (PEER, "Larsen", false, true));
        assert_ne!(mine.sender.id, theirs.sender.id);
    }

    #[test]
    fn message_generation_decoder_accepts_legacy_absence_but_never_coerces_present_invalid_metadata() {
        let legacy: WireMessage = serde_json::from_value(json!({"id":"legacy"})).unwrap();
        assert!(legacy.generation.is_none());
        for generation in [json!(null), json!(-1), json!("1"), json!(u64::MAX)] {
            assert!(serde_json::from_value::<WireMessage>(json!({"id":"bad", "generation":generation})).is_err());
        }
        assert!(serde_json::from_value::<WireMessage>(json!({"id":"bad", "generation":1, "group_id":"wrong"})).is_err());
    }

    #[test]
    fn legacy_saved_attribution_comes_from_preexisting_local_content_not_the_current_wire_claim() {
        let saved = saved("http://unused");
        let wire: WireMessage = serde_json::from_value(json!({"id":"legacy", "author_id":ME, "e2ee":true, "created_at":WHEN})).unwrap();
        let mut opened = e2ee::Opened { text:"saved words".into(), ..e2ee::Opened::default() };
        let mut previous = Message::plain("legacy", "Bea", "saved words", 456, false);
        previous.sender.id = PEER.to_owned(); previous.seal = Seal::Encrypted;
        assert!(opened.keep_saved_attribution(&previous));
        let reads = HashMap::from([("legacy".to_owned(), Read::Legacy(opened))]);
        let mapped = map_message(&saved, &HashMap::new(), &wire, &reads, &mut Vec::new()).unwrap();
        assert_eq!(mapped.sender.name, "Bea");
        assert_eq!(mapped.sender.id, PEER);
        assert!(!mapped.outgoing);
        assert!(mapped.saved_history);
        assert_eq!(mapped.sent, 456);
    }

    /// `channel.updated` is broadcast with the actor's own unread baked into `app_channel_json`.
    /// Applying that count clears everyone else's badge. The viewer's count comes from the list.
    #[test]
    fn a_channel_update_does_not_apply_someone_elses_unread() {
        let (_adapter, ctx, saved) = fixture(|_| None);
        assert_eq!(ctx.known("channel-1").unwrap().unwrap().unread, 2);
        let frame = json!({
            "channel": {
                "id": "channel-1",
                "name": "general",
                "topic": "Moved",
                "category_id": "cat-1",
                "category_name": "Text",
                "position": 1,
                "unread": 0,
                "last_message": {
                    "id": "c-10",
                    "author_id": PEER,
                    "body": "later",
                    "created_at": "2024-01-02T03:05:05Z"
                }
            }
        });
        on_live(&ctx, &saved, &HashMap::new(), "channel.updated", &frame);
        let channel = ctx.known("channel-1").unwrap().unwrap();
        assert_eq!(channel.unread, 2, "a channel update carries the actor's unread, not ours");
        assert_eq!(channel.topic, "Moved");
        assert_eq!(channel.preview, "later");
    }

    /// A 403 on the channel list is a refusal, not an empty server. Treating it as empty makes
    /// the next tree replace delete every channel's stored messages.
    #[test]
    fn a_refused_channel_list_does_not_drop_stored_messages() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/channels ") && seen.fetch_add(1, Ordering::Relaxed) > 0 {
                return Some(http(403, "Forbidden", r#"{"error":"forbidden"}"#));
            }
            None
        });
        let kept = Message::plain("m-kept", "Ada", "kept", 1_700_000_000, false);
        ctx.hub.store().ingest(&ctx.key("channel-1"), &kept, false).unwrap();
        refresh(&ctx, &saved);
        let page = ctx
            .hub
            .store()
            .messages(&ctx.key("channel-1"), &Page::Newest, 10, 1_800_000_000)
            .unwrap()
            .expect("a refused channel list leaves the channel");
        assert_eq!(
            page.messages.iter().find(|message| message.id == "m-kept").map(|message| message.text.as_str()),
            Some("kept")
        );
    }

    /// A 200 with empty channels and directs is still an empty tree replace. Keeping the stored
    /// tree leaves messages that a server blip would otherwise delete.
    #[test]
    fn an_empty_channel_list_does_not_drop_stored_messages() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/channels ") {
                if seen.fetch_add(1, Ordering::Relaxed) >= 1 {
                    return Some(http(200, "OK", r#"{"channels":[]}"#));
                }
                return None;
            }
            if request.starts_with("GET /v1/directs ") && seen.load(Ordering::Relaxed) >= 2 {
                return Some(http(200, "OK", r#"{"directs":[]}"#));
            }
            None
        });
        let kept = Message::plain("m-kept", "Ada", "kept", 1_700_000_000, false);
        ctx.hub.store().ingest(&ctx.key("channel-1"), &kept, false).unwrap();
        refresh(&ctx, &saved);
        let page = ctx
            .hub
            .store()
            .messages(&ctx.key("channel-1"), &Page::Newest, 10, 1_800_000_000)
            .unwrap()
            .expect("an empty channel list leaves the channel");
        assert_eq!(
            page.messages.iter().find(|message| message.id == "m-kept").map(|message| message.text.as_str()),
            Some("kept")
        );
    }

    /// An empty directs list with channels still present would delete every DM and its messages.
    #[test]
    fn an_empty_direct_list_does_not_drop_stored_messages() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/directs ") && seen.fetch_add(1, Ordering::Relaxed) >= 1 {
                return Some(http(200, "OK", r#"{"directs":[]}"#));
            }
            None
        });
        let kept = Message::plain("m-dm", "Ada", "kept dm", 1_700_000_000, false);
        ctx.hub.store().ingest(&ctx.key("direct-1"), &kept, false).unwrap();
        refresh(&ctx, &saved);
        let page = ctx
            .hub
            .store()
            .messages(&ctx.key("direct-1"), &Page::Newest, 10, 1_800_000_000)
            .unwrap()
            .expect("an empty direct list leaves the direct");
        assert_eq!(
            page.messages.iter().find(|message| message.id == "m-dm").map(|message| message.text.as_str()),
            Some("kept dm")
        );
    }

    /// Directs sort after channels. A paged tree of the first page can miss them and let an
    /// empty directs list delete every DM.
    #[test]
    fn space_text_kinds_finds_directs_past_the_first_tree_page() {
        let (_adapter, ctx, _saved) = fixture(|_| None);
        let space = ctx.key(SPACE);
        let account = space.account();
        for index in 0..120 {
            let mut row = crate::store::ConversationRow::new(format!("ch-{index}"), ConversationKind::Channel, format!("#{index}"));
            row.space = Some(SPACE.to_owned());
            row.position = index;
            ctx.hub.store().upsert_conversation(&account, &row).unwrap();
        }
        let mut direct = crate::store::ConversationRow::new("dm-late", ConversationKind::Direct, "Bea");
        direct.space = Some(SPACE.to_owned());
        direct.position = DIRECTS_AT;
        ctx.hub.store().upsert_conversation(&account, &direct).unwrap();
        let page = ctx.hub.store().tree(&space, 0, PAGE_MAX).unwrap().unwrap();
        assert!(
            !page.conversations.iter().any(|row| row.kind == ConversationKind::Direct),
            "the first tree page is only channels"
        );
        // Fixture already stores room-1 as Voice.
        assert_eq!(ctx.hub.store().space_text_kinds(&space).unwrap(), (true, true, true));
    }

    /// An empty rooms list with voice rooms already stored would delete those rooms.
    #[test]
    fn an_empty_rooms_list_does_not_drop_stored_voice_rooms() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/rooms ") && seen.fetch_add(1, Ordering::Relaxed) >= 1 {
                return Some(http(200, "OK", "[]"));
            }
            None
        });
        assert!(ctx.known("room-1").unwrap().is_some(), "fixture stores a voice room");
        refresh(&ctx, &saved);
        assert!(
            ctx.known("room-1").unwrap().is_some(),
            "an empty rooms list leaves the stored voice room"
        );
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
        let (rows, _) = ctx
            .hub
            .store()
            .inbox(Some(Network::Tiny), InboxFilter::All, 0, 20, &[])
            .unwrap();
        assert!(rows.iter().any(|row| row.key.id == "direct-1"));
        assert_eq!(direct.call, CallSupport::None);
    }

    #[test]
    fn a_direct_is_callable_and_a_missed_call_is_a_quiet_line() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/capabilities ") {
                return Some(http(200, "OK", r#"{"features":["direct_calls"]}"#));
            }
            if request.starts_with("GET /v1/directs ") {
                return Some(http(
                    200,
                    "OK",
                    &format!(
                        r#"{{"directs":[{{"id":"direct-1","peer":{{"user_id":"{PEER}","display_name":"Bea"}},"unread":1,"last_read":"m-1","last_message":{{"id":"m-2","author_id":"{ME}","body":"mine","created_at":"{WHEN}"}},"last_call":{{"id":"ring-1","outcome":"missed","caller_id":"{PEER}","ended_at":"{WHEN}"}}}}]}}"#
                    ),
                ));
            }
            None
        });
        let direct = ctx.known("direct-1").unwrap().unwrap();
        assert_eq!(direct.call, CallSupport::Native);
        assert_eq!(direct.preview, "mine");
        assert!(direct.preview_mine);
        assert_eq!(direct.unread, 2);
        let stored = ctx.stored_message("direct-1", "ring-1").unwrap().unwrap();
        assert_eq!(stored.text, "Bea started a call.");
        assert_eq!(stored.kind, MessageKind::System);
        assert_eq!(stored.sender.id, PEER);
        assert!(!stored.outgoing);

        let again = json!({
            "direct_id": "direct-1",
            "ring_id": "ring-1",
            "caller_id": PEER,
            "callee_id": ME,
            "outcome": "missed",
            "ended_at": WHEN,
        });
        on_live(&ctx, &saved, &HashMap::new(), "direct.call", &again);
        assert_eq!(ctx.known("direct-1").unwrap().unwrap().unread, 2);

        let ended = json!({
            "direct_id": "direct-1",
            "ring_id": "ring-2",
            "caller_id": ME,
            "callee_id": PEER,
            "outcome": "ended",
            "ended_at": WHEN,
        });
        on_live(&ctx, &saved, &HashMap::new(), "direct.call", &ended);
        let mine = ctx.stored_message("direct-1", "ring-2").unwrap().unwrap();
        assert_eq!(mine.text, "Ada started a call.");
        assert!(mine.outgoing);
        assert_eq!(mine.kind, MessageKind::System);
        let after = ctx.known("direct-1").unwrap().unwrap();
        assert_eq!(after.preview, "mine");
        assert_eq!(after.unread, 2);
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
    fn an_open_backfill_reaches_the_running_tiny_adapter() {
        assert_eq!(super::prove_tiny_open_backfill(), Ok(()));
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
    fn messages_that_share_a_stamp_follow_seq_and_not_the_id() {
        // The id sorts the other way. The stamp is one second for both, which is all an
        // encrypted message ever has within its minute, and all a plaintext burst shares.
        let early = "ffffffff-ffff-ffff-ffff-ffffffffffff";
        let later = "00000000-0000-0000-0000-000000000001";
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/conversations/direct-1/messages?") && request.contains("limit=50") {
                return Some(http(200, "OK", &format!(
                    r#"{{"messages":[{{"id":"{later}","author_id":"{PEER}","body":"second","seq":2,"created_at":"{WHEN}"}},{{"id":"{early}","author_id":"{ME}","body":"first","seq":1,"created_at":"{WHEN}"}}]}}"#
                )));
            }
            None
        });
        load_page(&ctx, &saved, "direct-1", None, PAGE);
        let page = ctx.hub.store().messages(&ctx.key("direct-1"), &Page::Newest, 10, 1_800_000_000).unwrap().unwrap();
        let ids: Vec<&str> = page.messages.iter().map(|message| message.id.as_str()).collect();
        assert_eq!(ids, [early, later]);
        assert_eq!(page.messages[0].text, "first");
        assert_eq!(page.messages[1].text, "second");
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

    /// An Encrypted row with no opened text used to become a blank inbox preview.
    #[test]
    fn an_encrypted_row_with_no_opened_text_still_names_the_preview() {
        assert_eq!(super::sealed_preview_words(Seal::Encrypted, ""), "Encrypted message");
        assert_eq!(super::sealed_preview_words(Seal::Encrypted, "  "), "Encrypted message");
        assert_eq!(super::sealed_preview_words(Seal::Encrypted, "opened words"), "opened words");
        assert_eq!(super::prove_sealed_preview(), Ok(()));
        let (_adapter, ctx, saved) = fixture(|_| None);
        let mut message = Message::plain("s-empty", "Bea", "", 1_700_000_000, false);
        message.seal = Seal::Encrypted;
        ctx.hub.store().ingest(&ctx.key("direct-1"), &message, false).unwrap();
        assert_eq!(super::sealed_preview(&ctx, &saved, "direct-1", "s-empty"), "Encrypted message");
    }

    /// After a sqlite wipe the sealed cache can still hold what this device opened. The inbox
    /// preview must use that text, not the generic "Encrypted message" placeholder.
    #[test]
    fn a_wiped_store_still_names_the_preview_from_the_sealed_cache() {
        assert_eq!(super::prove_sealed_cache_preview(), Ok(()));
    }

    /// After a sqlite wipe, a captionless image and a captioned attachment in the sealed cache
    /// must rehydrate into the store with Seal::Encrypted and registered media.
    #[test]
    fn a_wiped_store_restores_cached_sealed_attachments() {
        assert_eq!(super::prove_sealed_preview_attachments(), Ok(()));
    }

    /// After an update the MLS device may not be started yet. A history fetch must still restore
    /// opened text from the sealed cache instead of writing "Can't be decrypted" stubs.
    #[test]
    fn a_history_fetch_restores_opened_text_from_the_sealed_cache_without_a_device() {
        assert_eq!(super::prove_cache_open_without_device(), Ok(()));
    }

    #[test]
    fn a_started_device_reads_cached_history_before_enrolment() {
        assert_eq!(super::prove_cache_before_enrol(), Ok(()));
    }

    /// A later undecryptable fetch keeps the text this device already opened.
    #[test]
    fn an_opened_sealed_row_is_kept_when_a_later_fetch_cannot_open_it() {
        assert_eq!(super::prove_keep_opened_sealed(), Ok(()));
    }

    /// A current server sets `e2ee`. One that sends ciphertext without that flag must not be
    /// stored as an empty plaintext row: the pane would draw no words at all.
    #[test]
    fn a_sealed_row_without_the_flag_is_not_stored_as_blank_plaintext() {
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/conversations/direct-1/messages?") {
                return Some(http(200, "OK", &format!(
                    r#"{{"messages":[{{"id":"s-2","author_id":"{PEER}","body":"","ciphertext":"AAAA","epoch":3,"device_id":"d-1","created_at":"{WHEN}"}}]}}"#
                )));
            }
            None
        });
        load_page(&ctx, &saved, "direct-1", None, PAGE);
        let stored = ctx.stored_message("direct-1", "s-2").unwrap().unwrap();
        assert_eq!(stored.seal, Seal::Undecryptable);
        assert_ne!(stored.text, "");
        assert_eq!(stored.text, protocol::UNDECRYPTABLE);
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
