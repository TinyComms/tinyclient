//! Text channels and direct messages on a Tiny server. Voice rooms stay voice channels.
//!
//! The shell already draws a conversation once a row is in the store, so this module only
//! maps the server's JSON into that store and answers the commands the shell already sends.
//! A GIF or a video is an attachment the existing viewer fetches. Reactions are Unicode emoji.

use super::e2ee::{self, Read};
use super::live::{Ask, Live};
use super::Saved;
use crate::adapters::{About, CatalogPage, Command, Context, ConversationAbout, Download, Members, Refusal};
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
    let caps = account_caps(saved);
    ctx.capabilities(caps);
    if caps.custom_emoji != protocol::EmojiScope::None || caps.stickers {
        sync_emoji(ctx, saved);
    }
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
    // An empty list is checked, not trusted blindly and not allowed to freeze the rest of the
    // tree. Stored channels/directs confirmed gone by 404 are removed; survivors (200, other
    // status, or transport error) are carried into `rows` so `replace_tree` keeps them and their
    // messages. Voice rooms have no per-room read endpoint, so stored voice rows are carried
    // forward when `/v1/rooms` is empty.
    let space = ctx.key(SPACE);
    let surviving_channels = if channels.is_empty() {
        let stored = ctx
            .hub
            .store()
            .space_rows_of_kinds(&space, &[ConversationKind::Channel, ConversationKind::Announcement])
            .unwrap_or_default();
        probe_survivors(ctx, saved, stored)
    } else {
        Vec::new()
    };
    let surviving_directs = if directs.is_empty() {
        let stored = ctx
            .hub
            .store()
            .space_rows_of_kinds(&space, &[ConversationKind::Direct])
            .unwrap_or_default();
        probe_survivors(ctx, saved, stored)
    } else {
        Vec::new()
    };
    let surviving_rooms = if rooms.is_empty() {
        ctx.hub
            .store()
            .space_rows_of_kinds(&space, &[ConversationKind::Voice])
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let callable = directs_callable(saved);
    let caps = super::manage::capabilities_of(saved);
    let has_pins = caps.has(protocol::TINY_FEATURE_PINS);
    let blocked_rows = if caps.has(protocol::TINY_FEATURE_BLOCKS) {
        optional_list::<WireBlock>(saved, "/v1/blocks", "blocks").unwrap_or_default()
    } else {
        Vec::new()
    };
    let blocked_ids: HashSet<String> = blocked_rows
        .iter()
        .filter(|b| super::safe_id(&b.user_id))
        .map(|b| b.user_id.clone())
        .collect();
    let mut people: Vec<PersonRow> = names
        .iter()
        .filter(|(id, _)| super::safe_id(id))
        .map(|(id, name)| PersonRow {
            id: id.clone(),
            name: name.clone(),
            ..PersonRow::default()
        })
        .collect();
    let category_map: HashMap<String, (String, i32)> = super::manage::categories_of(saved)
        .into_iter()
        .map(|cat| (cat.id, (cat.name, cat.position)))
        .collect();
    for block in &blocked_rows {
        if super::safe_id(&block.user_id) && !people.iter().any(|p| p.id == block.user_id) {
            people.push(PersonRow {
                id: block.user_id.clone(),
                name: title_or(&block.display_name, "Someone"),
                ..PersonRow::default()
            });
        }
    }
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
        row.moderate = channel.my_permissions.unwrap_or(0) & protocol::TINY_MANAGE_MESSAGES != 0;
        row.can_pin = has_pins && (channel.my_permissions.unwrap_or(0) & protocol::TINY_MANAGE_CHANNELS != 0);
        row.rights = protocol::ConversationRights {
            delete_others: row.moderate,
            pin: row.can_pin,
            ..protocol::ConversationRights::default()
        };
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
                let (name, position) = category_map
                    .get(category)
                    .filter(|(name, _)| !name.is_empty())
                    .map(|(name, pos)| (name.as_str(), *pos))
                    .unwrap_or_else(|| {
                        let position = channels
                            .iter()
                            .filter(|other| other.category_id.as_deref() == Some(category))
                            .map(|other| other.position)
                            .min()
                            .unwrap_or(0);
                        (category, position)
                    });
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
            .mentions(channel.mentions)
            .sealed(channel.e2ee),
        );
        rows.push(row);
    }
    rows.extend(surviving_channels);
    if !directs.is_empty() || !surviving_directs.is_empty() {
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
        row.can_pin = has_pins;
        row.rights.pin = has_pins;
        if blocked_ids.contains(&direct.peer.user_id) {
            row.filing = Some(crate::store::Filing {
                hidden: true,
                ..crate::store::Filing::default()
            });
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
            .mentions(direct.mentions)
            .sealed(direct.e2ee),
        );
        rows.push(row);
    }
    rows.extend(surviving_directs);
    if !rooms.is_empty() || !surviving_rooms.is_empty() {
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
    rows.extend(surviving_rooms);
    if !people.is_empty() {
        let _ = ctx.people(&people);
    }
    for id in &blocked_ids {
        let _ = ctx.relation(id, protocol::Relation::Blocked);
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
        "category.updated" => {
            if let Some(category) = frame.get("category") {
                let id = field(category, "id");
                let name = field(category, "name");
                let position = category.get("position").and_then(Value::as_i64).unwrap_or(0) as i32;
                if super::safe_id(id) && !name.is_empty() {
                    let _ = ctx.group(SPACE, &Group::new(id, name, position));
                }
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
                // Off the socket's thread: owe_reactions takes the device mutex, which
                // syncs and repairs can hold across network I/O.
                if e2ee::started(saved).is_some() {
                    let (ctx, saved, conversation) =
                        (ctx.clone(), saved.clone(), conversation.clone());
                    let _ = std::thread::Builder::new()
                        .name("tiny-e2ee-owed".into())
                        .spawn(move || owe_reactions(&ctx, &saved, &conversation));
                }
            }
        }
        "message.deleted" => {
            let conversation = nested_str(frame, "conversation_id");
            let message = nested_id(frame, "message", "message_id").unwrap_or("");
            let deleted_by = nested_str(frame, "deleted_by");
            if super::safe_id(conversation) && super::safe_id(message) {
                if !deleted_by.is_empty() {
                    let _ = ctx.deleted_by_moderator(conversation, message);
                } else {
                    let _ = ctx.deleted(conversation, message);
                }
            }
        }
        "pin.added" | "message.pinned" => {
            let conversation = nested_str(frame, "conversation_id");
            let message = nested_id(frame, "message", "message_id").unwrap_or("");
            if super::safe_id(conversation) && super::safe_id(message) {
                let _ = ctx.pinned(conversation, message, true);
            }
        }
        "pin.removed" | "message.unpinned" => {
            let conversation = nested_str(frame, "conversation_id");
            let message = nested_id(frame, "message", "message_id").unwrap_or("");
            if super::safe_id(conversation) && super::safe_id(message) {
                let _ = ctx.pinned(conversation, message, false);
            }
        }
        "block.updated" => {
            let user_id = field(frame, "user_id");
            let blocked = frame.get("blocked").and_then(Value::as_bool).unwrap_or(false);
            if super::safe_id(user_id) {
                let rel = if blocked {
                    protocol::Relation::Blocked
                } else {
                    protocol::Relation::None
                };
                let _ = ctx.relation(user_id, rel);
                hide_direct_for_peer(ctx, user_id, blocked);
            }
        }
        "reaction.updated" => merge_reaction(ctx, saved, frame),
        // The sealed kind: a reaction blob in an encrypted conversation, opaque to the server.
        // Off the socket's thread: the event only says the server's list moved; the list folds.
        "e2ee.reaction" => {
            let conversation_id = field(frame, "conversation_id");
            if super::safe_id(conversation_id) {
                if let Some(device) = e2ee::started(saved) {
                    let (conversation_id, ctx, saved) =
                        (conversation_id.to_owned(), ctx.clone(), saved.clone());
                    let _ = std::thread::Builder::new().name("tiny-e2ee".into()).spawn(move || {
                        // The list is paged without the device mutex: sends and the socket
                        // thread must not wait on the server.
                        let mut landed = e2ee::fold_reactions_unlocked(&device, &saved, &conversation_id);
                        let (mut applied, mut owed) = (Vec::new(), Vec::new());
                        {
                            let mut device = device.lock().unwrap_or_else(|err| err.into_inner());
                            landed.extend(device.owed_reactions(&conversation_id));
                            for reaction in &landed {
                                let pair = (reaction.message.clone(), reaction.emoji.clone());
                                if apply_reaction(&ctx, &conversation_id, reaction) {
                                    applied.push(pair);
                                } else {
                                    owed.push(pair);
                                }
                            }
                            device.settle_reactions(&conversation_id, &applied, &owed);
                        }
                    });
                }
            }
        }
        "read.updated" => read_updated(ctx, saved, frame),
        "thread.updated" => thread_updated(ctx, saved, frame),
        "emoji.updated" => { let _ = sync_emoji(ctx, saved); }
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
            | "pin.added"
            | "pin.removed"
            | "message.pinned"
            | "message.unpinned"
            | "block.updated"
            | "reaction.updated"
            | "e2ee.reaction"
            | "read.updated"
            | "thread.updated"
            | "emoji.updated"
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
        Command::OpenThreads(_) => open_threads(ctx, saved),
        Command::MarkRead { conversation_id, message_id } => mark_read(saved, &conversation_id, &message_id),
        Command::Send { conversation_id, client_id, text, reply_to } => send(ctx, saved, conversation_id, client_id, text, reply_to),
        Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
            send_with(ctx, saved, conversation_id, client_id, text, reply_to, *with);
        }
        Command::CancelSend { client_id, .. } => upload::cancel(&ctx.account, &client_id),
        Command::React { conversation_id, message_id, emoji, on, burst: _, reply } => react(ctx, saved, &conversation_id, &message_id, &emoji, on, reply),
        Command::ReactionRules { reply, .. } => {
            let _ = reply.send(Ok(ReactionRules::default()));
        }
        Command::Edit {
            conversation_id,
            message_id,
            text,
            spans,
            suppress_embeds: _,
            keep_attachments: _,
            reply,
        } => edit(ctx, saved, &conversation_id, &message_id, &text, &spans, reply),
        Command::Delete {
            conversation_id,
            message_id,
            scope,
            reply,
        } => {
            if scope == protocol::DeleteScope::Me {
                let _ = reply.send(Err(Refusal::Refused("Tiny only supports deleting for everyone.".to_owned())));
            } else {
                delete(ctx, saved, &conversation_id, &message_id, reply);
            }
        }
        Command::Pinned { conversation_id, reply } => pinned(ctx, saved, &conversation_id, reply),
        Command::Pin {
            conversation_id,
            message_id,
            pinned,
            reply,
            ..
        } => pin(ctx, saved, &conversation_id, &message_id, pinned, reply),
        Command::Search {
            query,
            scope,
            reply,
            ..
        } => search(ctx, saved, &query, scope, reply),
        Command::Relate {
            person_id,
            action,
            reply,
        } => relate(ctx, saved, &person_id, action, reply),
        Command::Typing { conversation_id, activity: _ } => {
            if super::safe_id(&conversation_id) {
                live.ask(Ask::Type { conversation_id });
            }
        }
        Command::SetPresence { presence, status, status_emoji: _, clear_at, reply } => {
            let known = super::manage::known(saved);
            let supported = if known.checked {
                known.capabilities.has(protocol::TINY_FEATURE_PRESENCE_STATUS)
            } else {
                super::manage::capabilities_of(saved).has(protocol::TINY_FEATURE_PRESENCE_STATUS)
            };
            if !supported {
                let _ = reply.send(Err(Refusal::Unsupported));
            } else {
                live.ask(Ask::SetPresence { presence, status, clear_at, reply });
            }
        }
        Command::StartDirect { person_id, reply } => start_direct(ctx, saved, &person_id, reply),
        Command::StartDirectCall { person_id, reply } => start_direct_intent(ctx, saved, &person_id, true, reply),
        Command::MembersRemove { person, reply, .. } => {
            let answer = match super::manage::kick(ctx.account.clone(), person, String::new()) {
                protocol::Response::Ok => Ok(()),
                protocol::Response::Error { detail, .. } => Err(Refusal::Refused(detail)),
                _ => Err(Refusal::Unsupported),
            };
            let _ = reply.send(answer);
        }
        Command::ConversationUpdate { conversation_id, title, topic, description, reply } => {
            let edit = protocol::TinyRoomEdit {
                name: title,
                topic: topic.or(description),
                ..protocol::TinyRoomEdit::default()
            };
            let answer = match super::manage::room_update(ctx.account.clone(), conversation_id, edit) {
                protocol::Response::Ok => Ok(()),
                protocol::Response::Error { detail, .. } => Err(Refusal::Refused(detail)),
                _ => Err(Refusal::Unsupported),
            };
            let _ = reply.send(answer);
        }
        Command::Catalog { kind, query, reply, .. } => match kind {
            CatalogKind::Emoji | CatalogKind::Stickers => {
                let want_kind = if kind == CatalogKind::Stickers { "sticker" } else { "emoji" };
                let known = super::manage::known(saved);
                let items_src = if known.emoji.is_empty()
                    && (server_has_feature(saved, protocol::TINY_FEATURE_CUSTOM_EMOJI)
                        || server_has_feature(saved, protocol::TINY_FEATURE_STICKERS))
                {
                    sync_emoji(ctx, saved)
                } else {
                    known.emoji
                };
                let q = query.trim().to_ascii_lowercase();
                let items = items_src
                    .into_iter()
                    .filter(|item| item.kind == want_kind && (q.is_empty() || item.name.to_ascii_lowercase().contains(&q)))
                    .map(|item| {
                        let file_id = emoji_media_id(&item.id);
                        let mut file = MediaFile::new(&file_id, if item.animated { MediaKind::Gif } else { MediaKind::Image });
                        if !item.path.is_empty() {
                            file.path = Some(item.path.clone());
                        }
                        protocol::CatalogItem {
                            id: if kind == CatalogKind::Stickers { item.id.clone() } else { file_id },
                            name: if kind == CatalogKind::Stickers { item.name } else { format!(":{}:", item.name) },
                            file: Some(file),
                            pack: join_host(&saved.base_url),
                            animated: item.animated,
                            set: String::new(),
                            locked: false,
                        }
                    })
                    .collect();
                let _ = reply.send(Ok(CatalogPage { items, cursor: None }));
            }
            CatalogKind::Gifs => {
                let _ = reply.send(Ok(CatalogPage::default()));
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
        Command::Members { conversation_id, offset, limit, reply } => {
            channel_members(ctx, saved, &conversation_id, offset, limit, reply);
        }
        Command::Profile { person_id, reply } | Command::ProfileIn { person_id, reply, .. } => {
            profile(saved, &person_id, reply);
        }
        Command::ConversationInfo { conversation_id, reply } => {
            conversation_info(ctx, saved, &conversation_id, reply);
        }
        Command::Report { target, reason, note, block: _, reply } => {
            let (kind, target_id) = match &target {
                protocol::ReportTarget::Message { message_id, .. } => ("message".to_owned(), message_id.clone()),
                protocol::ReportTarget::Person(key) => ("member".to_owned(), key.id.clone()),
                protocol::ReportTarget::Conversation(key) => ("channel".to_owned(), key.id.clone()),
            };
            let category = match reason {
                protocol::ReportReason::Spam => "spam",
                protocol::ReportReason::Harassment => "harassment",
                protocol::ReportReason::Violence => "violence",
                protocol::ReportReason::Illegal => "illegal",
                protocol::ReportReason::Impersonation => "impersonation",
                protocol::ReportReason::Other => "other",
            }
            .to_owned();
            let note = (!note.trim().is_empty()).then_some(note);
            let answer = match super::manage::report_create(ctx.account.clone(), kind, target_id, category, note, None) {
                protocol::Response::TinyReported { .. } => Ok(()),
                protocol::Response::Error { detail, .. } => Err(Refusal::Refused(detail)),
                _ => Err(Refusal::Unsupported),
            };
            let _ = reply.send(answer);
        }
        other => other.unsupported(ctx),
    }
}

struct Note {
    id: String,
    /// The conversation is end-to-end encrypted: its last line has no body for the server.
    sealed: bool,
    unread: u32,
    mentions: u32,
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
            mentions: 0,
            last_read: kept(last_read),
            peer_read: kept(peer_read),
            last: last.cloned(),
            mine,
        }
    }

    fn mentions(mut self, mentions: u32) -> Note {
        self.mentions = mentions;
        self
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
        let _ = ctx.counts(&self.id, self.unread, self.mentions, self.last_read.as_deref(), updated);
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
        let (name, position) = super::manage::categories_of(saved)
            .into_iter()
            .find(|cat| cat.id == category && !cat.name.is_empty())
            .map(|cat| (cat.name, cat.position))
            .unwrap_or_else(|| (category.to_owned(), channel.position));
        let _ = ctx.group(SPACE, &Group::new(category, &name, position));
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
    let (unread, mentions) = match ctx.known(&channel.id) {
        Ok(Some(existing)) => (existing.unread, existing.mentions),
        _ => (channel.unread, channel.mentions),
    };
    Note::from_last(&channel.id, unread, None, None, channel.last_message.as_ref(), false).mentions(mentions).sealed(channel.e2ee).apply(ctx, saved, names);
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
        Note::from_last(&direct.id, direct.unread, direct.last_read.as_deref(), direct.peer_read.as_deref(), direct.last_message.as_ref(), mine).mentions(direct.mentions).sealed(direct.e2ee).apply(ctx, saved, names);
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
    let Some(mut message) = map_message(saved, names, wire, &opened, &mut media) else {
        return;
    };
    apply_thread_summary(ctx, conversation_id, wire, &mut message);
    if !media.is_empty() {
        let _ = ctx.media(&media);
    }
    let sealed_thread = match opened.get(&wire.id) {
        Some(Read::Opened(o) | Read::Legacy(o)) => o.thread_id.as_deref(),
        _ => None,
    };
    let target_thread = wire.thread_id.as_deref().or(sealed_thread).filter(|id| super::safe_id(id));
    if let Some(root_id) = target_thread {
        let tid = thread_known(ctx, conversation_id, root_id);
        let _ = ctx.message(&tid, &message);
    } else {
        let _ = ctx.message(conversation_id, &message);
    }
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
        message.spans.clear();
        message.mentions_me = false;
        apply_mentions(&mut message, saved, &opened.mentions, true);
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
                ..e2ee::OpenedFile::default()
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
        message.reactions.push(Reaction { emoji: emoji.to_owned(), file: None, count, mine: mine.unwrap_or(false), who: Vec::new() });
    }
    let _ = ctx.message(conversation_id, &message);
}

/// One folded encrypted reaction, onto the stored message it landed on: the count and
/// ownership as this device folds everyone's blobs. `False` when the message is not stored
/// (yet): the fold keeps the pair owed, and it is applied when the message lands.
fn apply_reaction(ctx: &Context, conversation_id: &str, landed: &e2ee::ReactionLanded) -> bool {
    if !super::safe_id(&landed.message) || !unicode_emoji(&landed.emoji) {
        return false;
    }
    let Ok(Some(mut message)) = ctx.stored_message(conversation_id, &landed.message) else {
        return false;
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
        message.reactions.push(Reaction { emoji: landed.emoji.clone(), file: None, count: landed.count, mine: landed.mine, who: Vec::new() });
    }
    let _ = ctx.message(conversation_id, &message);
    true
}

/// Applies the folded reactions a message's arrival still owed: a blob can fold before the
/// message it lands on is stored.
fn owe_reactions(ctx: &Context, saved: &Saved, conversation_id: &str) {
    let Some(device) = e2ee::started(saved) else {
        return;
    };
    let mut device = device.lock().unwrap_or_else(|err| err.into_inner());
    let landed = device.owed_reactions(conversation_id);
    if landed.is_empty() {
        return;
    }
    let (mut applied, mut owed) = (Vec::new(), Vec::new());
    for reaction in &landed {
        let pair = (reaction.message.clone(), reaction.emoji.clone());
        if apply_reaction(ctx, conversation_id, reaction) {
            applied.push(pair);
        } else {
            owed.push(pair);
        }
    }
    device.settle_reactions(conversation_id, &applied, &owed);
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

fn split_thread_id(id: &str) -> Option<(&str, &str)> {
    let (channel, root) = id.split_once(':')?;
    if super::safe_id(channel) && super::safe_id(root) {
        Some((channel, root))
    } else {
        None
    }
}

fn thread_conv_id(channel_id: &str, root_id: &str) -> String {
    format!("{channel_id}:{root_id}")
}

fn thread_known(ctx: &Context, channel_id: &str, root_id: &str) -> String {
    let id = thread_conv_id(channel_id, root_id);
    if ctx.known(&id).ok().flatten().is_none() {
        let parent = ctx.known(channel_id).ok().flatten();
        let title = parent
            .as_ref()
            .map(|p| p.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Thread".to_owned());
        let mut row = ConversationRow::new(&id, ConversationKind::Thread, title);
        row.space = Some(SPACE.to_owned());
        row.parent = Some(channel_id.to_owned());
        row.root = Some(root_id.to_owned());
        if let Some(p) = parent {
            row.encryption = p.encryption;
        }
        let _ = ctx.conversation(&row);
    }
    id
}

fn map_thread_summary(ctx: &Context, channel_id: &str, root_id: &str, wire: &WireThreadSummary) -> protocol::ThreadSummary {
    let last = wire.last_reply_at.as_deref().and_then(rfc3339).unwrap_or(0);
    let participants = wire
        .participants
        .iter()
        .take(protocol::THREAD_FACES)
        .filter(|id| !id.is_empty())
        .map(|id| ctx.key(id))
        .collect();
    protocol::ThreadSummary {
        id: thread_conv_id(channel_id, root_id),
        replies: wire.replies,
        last,
        participants,
        unread: wire.unread,
    }
}

fn apply_thread_summary(ctx: &Context, channel_id: &str, wire: &WireMessage, message: &mut Message) {
    if let Some(summary) = wire.thread.as_ref().filter(|s| s.replies > 0) {
        message.thread = Some(map_thread_summary(ctx, channel_id, &wire.id, summary));
    }
}

fn thread_updated(ctx: &Context, _saved: &Saved, frame: &Value) {
    let channel_id = field(frame, "conversation_id");
    let root_id = field(frame, "message_id");
    if !super::safe_id(channel_id) || !super::safe_id(root_id) {
        return;
    }
    let summary = frame
        .get("thread")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<WireThreadSummary>(v.clone()).ok())
        .filter(|s| s.replies > 0)
        .map(|s| map_thread_summary(ctx, channel_id, root_id, &s));
    if let Ok(Some(mut root)) = ctx.stored_message(channel_id, root_id) {
        root.thread = summary.clone();
        let _ = ctx.message(channel_id, &root);
    }
    if let Some(summary) = summary {
        let tid = thread_known(ctx, channel_id, root_id);
        let _ = ctx.counts(&tid, summary.unread, 0, None, summary.last);
    }
}

fn open_threads(ctx: &Context, saved: &Saved) {
    if !server_has_feature(saved, protocol::TINY_FEATURE_THREADS) {
        return;
    }
    let Ok((200, body)) = super::asked("GET", saved, "/v1/threads", None) else {
        return;
    };
    let Ok(items) = serde_json::from_value::<Vec<WireFollowedThread>>(body) else {
        return;
    };
    let names = member_names(saved);
    for item in items {
        if !super::safe_id(&item.channel_id) || !super::safe_id(&item.message_id) {
            continue;
        }
        let tid = thread_known(ctx, &item.channel_id, &item.message_id);
        let _ = ctx.filing(&tid, &crate::store::Filing { followed: true, ..crate::store::Filing::default() });
        if let Some(wire) = item.message.as_ref() {
            let mut media = Vec::new();
            let opened = open_rows(ctx, saved, &item.channel_id, &[wire]);
            if let Some(mut msg) = map_message(saved, &names, wire, &opened, &mut media) {
                if let Some(summary) = item.thread.as_ref().filter(|s| s.replies > 0) {
                    msg.thread = Some(map_thread_summary(ctx, &item.channel_id, &item.message_id, summary));
                }
                if !media.is_empty() {
                    let _ = ctx.media(&media);
                }
                let _ = ctx.history(&tid, &[msg]);
            }
        }
        if let Some(summary) = item.thread.as_ref() {
            let last = summary.last_reply_at.as_deref().and_then(rfc3339).unwrap_or(0);
            let _ = ctx.counts(&tid, summary.unread, 0, None, last);
        }
    }
}

#[allow(dead_code)]
fn follow_thread(saved: &Saved, root_id: &str, follow: bool) -> Result<(), String> {
    if !super::safe_id(root_id) {
        return Err(super::CHECK.to_owned());
    }
    let method = if follow { "PUT" } else { "DELETE" };
    let path = format!("/v1/messages/{root_id}/thread/follow");
    match super::asked(method, saved, &path, None) {
        Ok((200 | 204, _)) => Ok(()),
        Ok((status, body)) => Err(super::explain(status, &body, super::Missing::Plain).to_owned()),
        Err(detail) => Err(detail.to_owned()),
    }
}

fn fetch_page(ctx: &Context, saved: &Saved, conversation_id: &str, before: Option<&str>, limit: u32) -> Result<(), String> {
    let thread = split_thread_id(conversation_id);
    let (path, e2ee_conv) = if let Some((channel_id, root_id)) = thread {
        thread_known(ctx, channel_id, root_id);
        let p = match before {
            Some(before) => format!("/v1/messages/{root_id}/thread?before={before}&limit={limit}"),
            None => format!("/v1/messages/{root_id}/thread?limit={limit}"),
        };
        (p, channel_id)
    } else {
        let threads_q = if server_has_feature(saved, protocol::TINY_FEATURE_THREADS) { "&threads=1" } else { "" };
        let p = match before {
            Some(before) => format!("/v1/conversations/{conversation_id}/messages?before={before}&limit={limit}{threads_q}"),
            None => format!("/v1/conversations/{conversation_id}/messages?limit={limit}{threads_q}"),
        };
        (p, conversation_id)
    };
    match super::asked("GET", saved, &path, None) {
        Ok((200, body)) => {
            let wires: Vec<WireMessage> = list_of(body, "messages").map_err(|()| super::UNREADABLE.to_owned())?;
            let names = member_names(saved);
            let mut media = Vec::new();
            let opened = open_rows(ctx, saved, e2ee_conv, &wires.iter().collect::<Vec<_>>());
            let messages: Vec<Message> = wires
                .iter()
                .filter(|wire| wire.kind.as_deref() != Some("edit"))
                .filter_map(|wire| {
                    let mut msg = map_message(saved, &names, wire, &opened, &mut media)?;
                    if thread.is_none() {
                        apply_thread_summary(ctx, conversation_id, wire, &mut msg);
                    }
                    Some(msg)
                })
                .collect();
            if !media.is_empty() {
                let _ = ctx.media(&media);
            }
            if !messages.is_empty() {
                ctx.history(conversation_id, &messages).map_err(|err| err.to_string())?;
            }
            // The reactions of an encrypted conversation are not rows of this page; the folded
            // ones that changed while this device was away are asked for now, with anything a
            // page before this one still owed. The list is paged without the device mutex,
            // so the network holds up no other user of this device.
            if thread.is_none() && sealed_here(ctx, saved, e2ee_conv) == Ok(true) {
                if let Some(device) = e2ee::started(saved) {
                    let mut landed = e2ee::fold_reactions_unlocked(&device, saved, e2ee_conv);
                    let mut device = device.lock().unwrap_or_else(|err| err.into_inner());
                    landed.extend(device.owed_reactions(e2ee_conv));
                    let (mut applied, mut owed) = (Vec::new(), Vec::new());
                    for reaction in &landed {
                        let pair = (reaction.message.clone(), reaction.emoji.clone());
                        if apply_reaction(ctx, e2ee_conv, reaction) {
                            applied.push(pair);
                        } else {
                            owed.push(pair);
                        }
                    }
                    device.settle_reactions(e2ee_conv, &applied, &owed);
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
    send_spans(ctx, saved, conversation_id, client_id, text, reply_to, Vec::new(), None, false);
}

#[allow(clippy::too_many_arguments)]
fn send_spans(
    ctx: &Context,
    saved: &Saved,
    conversation_id: String,
    client_id: String,
    text: String,
    reply_to: Option<String>,
    spans: Vec<protocol::Span>,
    sticker_id: Option<String>,
    suppress_embeds: bool,
) {
    if !super::safe_id(&conversation_id) && split_thread_id(&conversation_id).is_none() {
        let outcome = Err(super::CHECK.to_owned());
        ctx.sent(&conversation_id, &client_id, outcome);
        return;
    }
    let check_conv = split_thread_id(&conversation_id).map_or(conversation_id.as_str(), |(ch, _)| ch);
    // The same question the post asks, so a conversation the device knows is encrypted but the
    // list has not labelled yet still goes to the sealed queue and never holds up the plain one.
    let account = send_account(saved, sealed_here(ctx, saved, check_conv) == Ok(true));
    let (posting, saved) = (ctx.clone(), saved.clone());
    in_order(account, Box::new(move || {
        let _ = crate::sendtime::run(&client_id, || {
            let outcome = post_message_with(&posting, &saved, &conversation_id, &client_id, &text, reply_to.as_deref(), &[], &spans, sticker_id.as_deref(), suppress_embeds);
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

fn emoji_media_id(id: &str) -> String {
    format!("tiny-emoji-{id}")
}

fn strip_emoji_media_id(id: &str) -> &str {
    id.strip_prefix("tiny-emoji-").unwrap_or(id)
}

/// Fetches the server's custom emoji & sticker set and registers + downloads every image up front
/// so encrypted conversations never trigger on-demand fetches when rendering a message.
fn sync_emoji(ctx: &Context, saved: &Saved) -> Vec<protocol::TinyEmoji> {
    let items = super::manage::fetch_emoji_set(saved, &ctx.account);
    let media: Vec<(MediaFile, String)> = items
        .iter()
        .map(|item| {
            let mut file = MediaFile::new(emoji_media_id(&item.id), if item.animated { MediaKind::Gif } else { MediaKind::Image });
            if !item.path.is_empty() {
                file.path = Some(item.path.clone());
            }
            (file, format!("{}/v1/emoji/{}", saved.base_url.trim_end_matches('/'), item.id))
        })
        .collect();
    if !media.is_empty() {
        let _ = ctx.media(&media);
    }
    items
}

fn emoji_from_spans(text: &str, spans: &[protocol::Span]) -> Vec<e2ee_core::envelope::EmojiRef> {
    let mut out = Vec::new();
    for span in spans {
        if out.len() >= 50 {
            break;
        }
        let (start, end) = (span.start as usize, span.end as usize);
        if start >= end || end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        let protocol::Style::Emoji { file } = &span.style else {
            continue;
        };
        let id = strip_emoji_media_id(&file.id);
        if super::safe_id(id) {
            out.push(e2ee_core::envelope::EmojiRef {
                id: id.to_owned(),
                start: span.start,
                end: span.end,
            });
        }
    }
    out
}

fn server_has_feature(saved: &Saved, feature: &str) -> bool {
    let was = super::manage::known(saved);
    if was.checked {
        return was.capabilities.has(feature);
    }
    super::manage::learn(saved).capabilities.has(feature)
}

fn mentions_from_spans(text: &str, spans: &[protocol::Span]) -> Vec<e2ee_core::envelope::MentionRef> {
    let mut out = Vec::new();
    for span in spans {
        if out.len() >= 50 {
            break;
        }
        let (start, end) = (span.start as usize, span.end as usize);
        if start >= end || end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        let protocol::Style::Mention { kind, id } = &span.style else {
            continue;
        };
        let (kind_str, id_opt) = match kind {
            protocol::MentionKind::User if !id.is_empty() => ("user", Some(id.clone())),
            protocol::MentionKind::Role if !id.is_empty() => ("role", Some(id.clone())),
            protocol::MentionKind::Everyone => ("everyone", None),
            _ => continue,
        };
        out.push(e2ee_core::envelope::MentionRef {
            kind: kind_str.to_owned(),
            id: id_opt,
            start: span.start,
            end: span.end,
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn post_message(
    ctx: &Context,
    saved: &Saved,
    conversation_id: &str,
    client_id: &str,
    text: &str,
    reply_to: Option<&str>,
    attachment_ids: &[String],
    spans: &[protocol::Span],
    sticker_id: Option<&str>,
) -> Result<Message, String> {
    post_message_with(ctx, saved, conversation_id, client_id, text, reply_to, attachment_ids, spans, sticker_id, false)
}

#[allow(clippy::too_many_arguments)]
fn post_message_with(
    ctx: &Context,
    saved: &Saved,
    conversation_id: &str,
    client_id: &str,
    text: &str,
    reply_to: Option<&str>,
    attachment_ids: &[String],
    spans: &[protocol::Span],
    sticker_id: Option<&str>,
    suppress_embeds: bool,
) -> Result<Message, String> {
    let (target_conv, thread_root) = match split_thread_id(conversation_id) {
        Some((ch, root)) => {
            thread_known(ctx, ch, root);
            (ch, Some(root))
        }
        None => {
            if !super::safe_id(conversation_id) {
                return Err(super::CHECK.to_owned());
            }
            (conversation_id, None)
        }
    };
    if sealed_here(ctx, saved, target_conv)? {
        return post_sealed(ctx, saved, conversation_id, client_id, text, reply_to, Vec::new(), attachment_ids, spans, sticker_id, suppress_embeds);
    }
    let mut body = serde_json::Map::new();
    body.insert("body".to_owned(), Value::String(text.to_owned()));
    body.insert("client_id".to_owned(), Value::String(client_id.to_owned()));
    if let Some(reply_to) = reply_to.filter(|id| super::safe_id(id)) {
        body.insert("reply_to".to_owned(), Value::String(reply_to.to_owned()));
    }
    if let Some(root_id) = thread_root {
        body.insert("thread_id".to_owned(), Value::String(root_id.to_owned()));
    }
    if !attachment_ids.is_empty() {
        body.insert("attachment_ids".to_owned(), json!(attachment_ids));
    }
    let mentions = mentions_from_spans(text, spans);
    if !mentions.is_empty() && server_has_feature(saved, protocol::TINY_FEATURE_MENTIONS) {
        body.insert("mentions".to_owned(), json!(mentions));
    }
    let emoji = emoji_from_spans(text, spans);
    if !emoji.is_empty() && server_has_feature(saved, protocol::TINY_FEATURE_CUSTOM_EMOJI) {
        body.insert("emoji".to_owned(), json!(emoji));
    }
    if let Some(sticker_id) = sticker_id.filter(|id| super::safe_id(id)) {
        if server_has_feature(saved, protocol::TINY_FEATURE_STICKERS) {
            body.insert("sticker_id".to_owned(), Value::String(sticker_id.to_owned()));
        }
    }
    if !suppress_embeds && server_has_feature(saved, protocol::TINY_FEATURE_LINK_PREVIEWS) {
        if let Some(preview_json) = fetch_plain_preview(saved, text) {
            body.insert("previews".to_owned(), Value::Array(vec![preview_json]));
        }
    }
    let path = format!("/v1/conversations/{target_conv}/messages");
    let answer = super::asked("POST", saved, &path, Some(Value::Object(body)));
    crate::sendtime::stage("post");
    match answer {
        Ok((200 | 201, value)) => accept_message(ctx, saved, target_conv, &value),
        Ok((status, value)) => Err(super::explain(status, &value, super::Missing::Plain).to_owned()),
        Err(detail) => Err(detail.to_owned()),
    }
}

fn upload_media_bytes_plain(saved: &Saved, name: &str, mime: &str, bytes: &[u8]) -> Option<String> {
    let boundary = format!("----TinyPreview{}", std::process::id());
    let mut body = Vec::with_capacity(bytes.len() + 256);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let url = super::endpoint(&saved.base_url, "/v1/media");
    let resp = super::agent()
        .post(&url)
        .header("Authorization", &format!("Bearer {}", saved.token))
        .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send(body.as_slice())
        .ok()?;
    if !(200..300).contains(&resp.status().as_u16()) {
        return None;
    }
    let text = resp.into_body().read_to_string().ok()?;
    let val: Value = serde_json::from_str(&text).ok()?;
    let id = val
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| val.pointer("/media/id").and_then(Value::as_str))?;
    super::safe_id(id).then(|| id.to_owned())
}

fn fetch_plain_preview(saved: &Saved, text: &str) -> Option<Value> {
    let url = super::preview::first_https_url(text)?;
    let fetched = super::preview::fetch_preview(&url)?;
    let image_id = fetched
        .image
        .as_ref()
        .and_then(|img| upload_media_bytes_plain(saved, "preview.jpg", "image/jpeg", &img.jpeg));
    let mut obj = serde_json::Map::new();
    obj.insert("url".to_owned(), Value::String(fetched.url));
    if !fetched.site.is_empty() {
        obj.insert("site".to_owned(), Value::String(fetched.site));
    }
    if !fetched.title.is_empty() {
        obj.insert("title".to_owned(), Value::String(fetched.title));
    }
    if !fetched.description.is_empty() {
        obj.insert("description".to_owned(), Value::String(fetched.description));
    }
    if let Some(id) = image_id {
        obj.insert("image_attachment_id".to_owned(), Value::String(id));
    }
    Some(Value::Object(obj))
}

fn fetch_sealed_preview(saved: &Saved, conversation_id: &str, text: &str) -> Option<(e2ee_core::envelope::PreviewRef, Option<String>)> {
    let url = super::preview::first_https_url(text)?;
    let fetched = super::preview::fetch_preview(&url)?;
    let mut img_ref = None;
    let mut img_id = None;
    if let Some(img) = fetched.image {
        if let Ok(mut tmp) = tempfile::NamedTempFile::new() {
            if std::io::Write::write_all(&mut tmp, &img.jpeg).is_ok() {
                if let Ok(sealed) = super::sealing::seal(tmp.path(), "preview.jpg", "image/jpeg", true, conversation_id) {
                    if let Ok(sealed_bytes) = std::fs::read(sealed.file.path()) {
                        let boundary = format!("----TinySealedPrev{}", std::process::id());
                        let mut body = Vec::with_capacity(sealed_bytes.len() + 512);
                        body.extend_from_slice(
                            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"sealed\"\r\n\r\n1\r\n").as_bytes(),
                        );
                        body.extend_from_slice(
                            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"id\"\r\n\r\n{}\r\n", sealed.entry.id).as_bytes(),
                        );
                        body.extend_from_slice(
                            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"sealed.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
                        );
                        body.extend_from_slice(&sealed_bytes);
                        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
                        let endpoint = super::endpoint(&saved.base_url, "/v1/media");
                        if let Ok(resp) = super::agent()
                            .post(&endpoint)
                            .header("Authorization", &format!("Bearer {}", saved.token))
                            .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
                            .send(body.as_slice())
                        {
                            if (200..300).contains(&resp.status().as_u16()) {
                                let mut entry = sealed.entry;
                                entry.width = img.width;
                                entry.height = img.height;
                                img_id = Some(entry.id.clone());
                                img_ref = Some(entry);
                            }
                        }
                    }
                }
            }
        }
    }
    Some((
        e2ee_core::envelope::PreviewRef {
            url: fetched.url,
            site: fetched.site,
            title: fetched.title,
            description: fetched.description,
            image: img_ref,
        },
        img_id,
    ))
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
fn post_sealed(
    ctx: &Context,
    saved: &Saved,
    conversation_id: &str,
    client_id: &str,
    text: &str,
    reply_to: Option<&str>,
    attachments: Vec<e2ee_core::AttachmentRef>,
    attachment_ids: &[String],
    spans: &[protocol::Span],
    sticker_id: Option<&str>,
    suppress_embeds: bool,
) -> Result<Message, String> {
    let (target_conv, thread_root) = match split_thread_id(conversation_id) {
        Some((ch, root)) => {
            thread_known(ctx, ch, root);
            (ch, Some(root))
        }
        None => (conversation_id, None),
    };
    let device = e2ee::started(saved).ok_or(NOT_SET_UP)?;
    let mut all_ids = attachment_ids.to_vec();
    let mut previews = Vec::new();
    if !suppress_embeds && server_has_feature(saved, protocol::TINY_FEATURE_LINK_PREVIEWS) {
        if let Some((preview, img_id)) = fetch_sealed_preview(saved, target_conv, text) {
            if let Some(id) = img_id {
                all_ids.push(id);
            }
            previews.push(preview);
        }
    }
    let mut envelope = e2ee_core::Envelope::message(text);
    envelope.reply_to = reply_to.filter(|id| super::safe_id(id)).map(str::to_owned);
    envelope.thread_id = thread_root.map(str::to_owned);
    envelope.attachments = attachments;
    envelope.mentions = mentions_from_spans(text, spans);
    envelope.emoji = emoji_from_spans(text, spans);
    envelope.sticker_id = sticker_id.filter(|id| super::safe_id(id)).map(str::to_owned);
    envelope.previews = previews;
    envelope.sent_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|since| since.as_millis() as i64);
    let answer = e2ee::send(&device, target_conv, client_id, e2ee_core::Kind::Message, None, &envelope, &all_ids)?;
    accept_message(ctx, saved, target_conv, &answer)
}

fn send_with(ctx: &Context, saved: &Saved, conversation_id: String, client_id: String, text: String, reply_to: Option<String>, outgoing: Outgoing) {
    let sticker_id = outgoing.pick.as_ref().filter(|p| p.kind == CatalogKind::Stickers).map(|p| strip_emoji_media_id(&p.id).to_owned());
    let suppress_embeds = outgoing.suppress_embeds;
    if outgoing.files.is_empty() {
        send_spans(ctx, saved, conversation_id, client_id, text, reply_to, outgoing.spans, sticker_id, suppress_embeds);
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
    let e2ee_conv = split_thread_id(&conversation_id).map_or(conversation_id.as_str(), |(ch, _)| ch).to_owned();
    let sealed = match sealed_here(ctx, saved, &e2ee_conv) {
        Ok(sealed) => sealed,
        Err(why) => {
            ctx.sent(&conversation_id, &client_id, Err(why));
            return;
        }
    };
    let saved = saved.clone();
    let posting = ctx.clone();
    let mut files = outgoing.files;
    let spans = outgoing.spans;
    let voice = outgoing.voice;
    let voice_duration_secs = voice.as_ref().map(|v| {
        if v.duration == 0 {
            0
        } else {
            ((v.duration + 500) / 1000).max(1)
        }
    });
    // tinyserver sniffs Ogg-Opus as `audio/ogg` and requires `.ogg`/`.oga` extension to match.
    if voice.is_some() {
        if let Some(last) = files.last_mut() {
            let current = if last.name.is_empty() {
                std::path::Path::new(&last.path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("voice.ogg")
                    .to_owned()
            } else {
                last.name.clone()
            };
            if current.to_ascii_lowercase().ends_with(".opus") {
                let stem = &current[..current.len() - 5];
                last.name = format!("{}.ogg", if stem.is_empty() { "voice" } else { stem });
            }
        }
    }
    let posting_id = conversation_id.clone();
    let posting_client = client_id.clone();
    if sealed {
        upload::spawn(ctx, &conversation_id, &client_id, &files, move |upload| {
            let mut refs = super::sealing::upload_sealed(upload, &saved, &e2ee_conv)?;
            if let (Some(voice), Some(last)) = (voice.as_ref(), refs.last_mut()) {
                last.duration = voice_duration_secs.unwrap_or(0);
                last.voice = true;
                last.waveform = voice.waveform.clone();
            }
            upload.check()?;
            let ids: Vec<String> = refs.iter().map(|file| file.id.clone()).collect();
            post_sealed(&posting, &saved, &posting_id, &posting_client, &text, reply_to.as_deref(), refs, &ids, &spans, None, suppress_embeds)
        });
        return;
    }
    upload::spawn(ctx, &conversation_id, &client_id, &files, move |upload| {
        let mut ids = Vec::with_capacity(upload.files().len());
        let last_ix = upload.files().len().saturating_sub(1);
        for at in 0..upload.files().len() {
            upload.check()?;
            let target = Target::post(super::endpoint(&saved.base_url, "/v1/media")).header("Authorization", &format!("Bearer {}", saved.token));
            let dur_str = voice_duration_secs.filter(|&d| d > 0 && at == last_ix).map(|d| d.to_string());
            let fields: Vec<(&str, &str)> = dur_str.as_deref().map(|d| vec![("duration", d)]).unwrap_or_default();
            let answer = match upload.send_form(at, &target, "file", &fields) {
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
        post_message_with(&posting, &saved, &posting_id, &posting_client, &text, reply_to.as_deref(), &ids, &spans, None, suppress_embeds)
    });
}

fn react(ctx: &Context, saved: &Saved, conversation_id: &str, message_id: &str, emoji: &str, on: bool, reply: crate::adapters::Reply<()>) {
    let custom_id = if unicode_emoji(emoji) {
        None
    } else {
        let raw = strip_emoji_media_id(emoji);
        let known = super::manage::known(saved);
        known.emoji.iter().find(|e| e.kind == "emoji" && (e.id == raw || format!(":{}:", e.name) == emoji)).map(|e| e.id.clone())
    };
    if custom_id.is_none() && !unicode_emoji(emoji) {
        let _ = reply.send(Err(Refusal::Refused("That reaction isn't allowed here.".to_owned())));
        return;
    }
    if !super::safe_id(message_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    let e2ee_conv = split_thread_id(conversation_id).map_or(conversation_id, |(ch, _)| ch);
    match sealed_here(ctx, saved, e2ee_conv) {
        Ok(true) => {
            let result = e2ee::started(saved)
                .ok_or(NOT_SET_UP.to_owned())
                .and_then(|shared| {
                    let landed = shared
                        .lock()
                        .unwrap_or_else(|err| err.into_inner())
                        .react(e2ee_conv, message_id, emoji, on)?;
                    Ok((shared, landed))
                });
            match result {
                Ok((device, landed)) => {
                    {
                        let mut locked = device.lock().unwrap_or_else(|err| err.into_inner());
                        let mut owed = Vec::new();
                        if let Some(landed) = &landed {
                            if !apply_reaction(ctx, conversation_id, landed) {
                                // The message is not stored yet (a reaction from a
                                // notification, say): the fold already knows, so keep the
                                // pair owed for when the message lands.
                                owed.push((landed.message.clone(), landed.emoji.clone()));
                            }
                        }
                        locked.settle_reactions(e2ee_conv, &[], &owed);
                    }
                    drop(device);
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
    let body = match custom_id {
        Some(id) => json!({ "emoji_id": id }),
        None => json!({ "emoji": emoji }),
    };
    let answer = super::asked(method, saved, &path, Some(body));
    answer_ok(answer, reply);
}

fn edit(ctx: &Context, saved: &Saved, conversation_id: &str, message_id: &str, text: &str, spans: &[protocol::Span], reply: crate::adapters::Reply<()>) {
    if !super::safe_id(message_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    let e2ee_conv = split_thread_id(conversation_id).map_or(conversation_id, |(ch, _)| ch);
    match sealed_here(ctx, saved, e2ee_conv) {
        Ok(true) => {
            let _ = reply.send(edit_sealed(ctx, saved, conversation_id, e2ee_conv, message_id, text, spans).map_err(Refusal::Refused));
            return;
        }
        Ok(false) => {}
        Err(why) => {
            let _ = reply.send(Err(Refusal::Refused(why)));
            return;
        }
    }
    let path = format!("/v1/messages/{message_id}");
    let mut payload = serde_json::Map::new();
    payload.insert("body".to_owned(), Value::String(text.to_owned()));
    if server_has_feature(saved, protocol::TINY_FEATURE_MENTIONS) {
        let mentions = mentions_from_spans(text, spans);
        payload.insert("mentions".to_owned(), json!(mentions));
    }
    match super::asked("PATCH", saved, &path, Some(Value::Object(payload))) {
        Ok((200 | 201, value)) => {
            if let Ok(message) = accept_message(ctx, saved, conversation_id, &value) {
                let _ = ctx.message(conversation_id, &message);
            } else if let Ok(Some(mut message)) = ctx.stored_message(conversation_id, message_id) {
                message.text = text.to_owned();
                message.edited = true;
                message.spans.clear();
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
fn edit_sealed(ctx: &Context, saved: &Saved, conversation_id: &str, e2ee_conv: &str, message_id: &str, text: &str, spans: &[protocol::Span]) -> Result<(), String> {
    let device = e2ee::started(saved).ok_or(NOT_SET_UP)?;
    let mut device = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut envelope = e2ee_core::Envelope::message(text);
    envelope.attachments = device.editable_attachments(e2ee_conv, message_id)?;
    envelope.mentions = mentions_from_spans(text, spans);
    let client_id = format!("edit-{message_id}-{}", crate::hub::now());
    device.post(e2ee_conv, &client_id, e2ee_core::Kind::Edit, Some(message_id), &envelope, &[])?;
    drop(device);
    if let Ok(Some(mut message)) = ctx.stored_message(conversation_id, message_id) {
        message.text = text.to_owned();
        message.edited = true;
        message.spans.clear();
        message.mentions_me = false;
        apply_mentions(&mut message, saved, &envelope.mentions, true);
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

fn pinned(
    ctx: &Context,
    saved: &Saved,
    conversation_id: &str,
    reply: crate::adapters::Reply<Vec<Message>>,
) {
    if !super::safe_id(conversation_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    let path = format!("/v1/conversations/{conversation_id}/pins");
    match super::asked("GET", saved, &path, None) {
        Ok((200, body)) => {
            let Ok(wires) = list_of::<WireMessage>(body, "messages") else {
                let _ = reply.send(Err(Refusal::Refused(super::UNREADABLE.to_owned())));
                return;
            };
            let names = member_names(saved);
            let mut media = Vec::new();
            let opened = open_rows(ctx, saved, conversation_id, &wires.iter().collect::<Vec<_>>());
            let mut messages = Vec::with_capacity(wires.len());
            for wire in &wires {
                let Some(mut msg) = map_message(saved, &names, wire, &opened, &mut media) else {
                    continue;
                };
                msg.pinned = true;
                if row_is_sealed(wire) && !matches!(opened.get(&wire.id), Some(Read::Opened(_) | Read::Legacy(_))) {
                    msg.text = "Pinned message isn't available on this device".to_owned();
                }
                messages.push(msg);
            }
            if !media.is_empty() {
                let _ = ctx.media(&media);
            }
            let _ = reply.send(Ok(messages));
        }
        Ok((404, _)) => {
            let _ = reply.send(Err(Refusal::Unsupported));
        }
        Ok((status, body)) => {
            let _ = reply.send(Err(Refusal::Refused(super::explain(status, &body, super::Missing::Plain).to_owned())));
        }
        Err(detail) => {
            let _ = reply.send(Err(Refusal::Refused(detail.to_owned())));
        }
    }
}

fn pin(
    ctx: &Context,
    saved: &Saved,
    conversation_id: &str,
    message_id: &str,
    on: bool,
    reply: crate::adapters::Reply<()>,
) {
    if !super::safe_id(conversation_id) || !super::safe_id(message_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    let method = if on { "PUT" } else { "DELETE" };
    let path = format!("/v1/messages/{message_id}/pin");
    match super::asked(method, saved, &path, None) {
        Ok((200 | 204, _)) => {
            let _ = ctx.pinned(conversation_id, message_id, on);
            let _ = reply.send(Ok(()));
        }
        other => answer_ok(other, reply),
    }
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[derive(Deserialize)]
struct WireSearchHit {
    conversation_id: String,
    message: WireMessage,
}

#[derive(Deserialize)]
struct WireSearchResponse {
    #[serde(default)]
    hits: Vec<WireSearchHit>,
    #[serde(default)]
    skipped_encrypted: bool,
}

fn search(
    ctx: &Context,
    saved: &Saved,
    query: &str,
    scope: crate::adapters::SearchScope,
    reply: crate::adapters::Reply<Vec<protocol::MessageHit>>,
) {
    let trimmed = query.trim();
    if trimmed.len() < 2 {
        let _ = reply.send(Ok(Vec::new()));
        return;
    }
    let mut path = format!("/v1/search?q={}&limit=25", percent_encode(trimmed));
    match &scope {
        crate::adapters::SearchScope::Conversation(conv_id) => {
            if !super::safe_id(conv_id) {
                let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
                return;
            }
            // Encrypted conversations (or ones whose encryption state cannot be verified locally)
            // must never send search queries to the server; answer from local cache only.
            if sealed_here(ctx, saved, conv_id) != Ok(false) {
                let key = ctx.key(conv_id);
                let hits = ctx
                    .hub
                    .store()
                    .search_messages(Some(&key), None, None, trimmed, 25)
                    .unwrap_or_default();
                let _ = reply.send(Ok(hits));
                return;
            }
            path.push_str("&conversation_id=");
            path.push_str(conv_id);
        }
        crate::adapters::SearchScope::Space(_) | crate::adapters::SearchScope::Account => {}
    }
    match super::asked("GET", saved, &path, None) {
        Ok((200, body)) => {
            let Ok(parsed) = serde_json::from_value::<WireSearchResponse>(body) else {
                let _ = reply.send(Err(Refusal::Refused(super::UNREADABLE.to_owned())));
                return;
            };
            let names = member_names(saved);
            let space_title = join_host(&saved.base_url);
            let mut hits = Vec::with_capacity(parsed.hits.len());
            let mut seen = HashSet::new();
            for item in parsed.hits {
                if !super::safe_id(&item.conversation_id) || !super::safe_id(&item.message.id) {
                    continue;
                }
                let conv_key = ctx.key(&item.conversation_id);
                let title = ctx
                    .known(&item.conversation_id)
                    .ok()
                    .flatten()
                    .map(|row| row.title)
                    .unwrap_or_default();
                let snippet: String = item
                    .message
                    .body
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(200)
                    .collect();
                let sent = rfc3339(&item.message.created_at).unwrap_or(0);
                let sender = name_of(&names, saved, &item.message.author_id);
                seen.insert((conv_key.clone(), item.message.id.clone()));
                hits.push(protocol::MessageHit {
                    conversation: conv_key,
                    message_id: item.message.id,
                    sent,
                    sender,
                    outgoing: item.message.author_id == saved.user_id,
                    snippet,
                    title,
                    space_title: space_title.clone(),
                    age_restricted: false,
                });
            }
            // If the server skipped encrypted conversations in a space-wide search, merge cached hits
            // from encrypted conversations on this device.
            if parsed.skipped_encrypted && matches!(scope, crate::adapters::SearchScope::Space(_)) {
                let space_key = ctx.key(SPACE);
                if let Ok(local) = ctx
                    .hub
                    .store()
                    .search_messages(None, None, Some(&space_key), trimmed, 25)
                {
                    for hit in local {
                        if seen.insert((hit.conversation.clone(), hit.message_id.clone())) {
                            hits.push(hit);
                        }
                    }
                    hits.sort_by(|a, b| b.sent.cmp(&a.sent));
                }
            }
            let _ = reply.send(Ok(hits));
        }
        Ok((404, _)) => {
            let _ = reply.send(Err(Refusal::Unsupported));
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

#[derive(Clone, Deserialize)]
struct WireBlock {
    user_id: String,
    #[serde(default)]
    display_name: String,
}

fn hide_direct_for_peer(ctx: &Context, user_id: &str, hidden: bool) {
    let space = ctx.key(SPACE);
    let directs = ctx
        .hub
        .store()
        .space_rows_of_kinds(&space, &[ConversationKind::Direct])
        .unwrap_or_default();
    for row in directs {
        if row.peer.as_deref() == Some(user_id) {
            let _ = ctx.filing(
                &row.id,
                &crate::store::Filing {
                    hidden,
                    ..crate::store::Filing::default()
                },
            );
        }
    }
}

fn relate(
    ctx: &Context,
    saved: &Saved,
    person_id: &str,
    action: protocol::RelateAction,
    reply: crate::adapters::Reply<()>,
) {
    if !super::safe_id(person_id) {
        let _ = reply.send(Err(Refusal::Refused(super::CHECK.to_owned())));
        return;
    }
    let (method, blocked) = match action {
        protocol::RelateAction::Block => ("PUT", true),
        protocol::RelateAction::Unblock => ("DELETE", false),
        _ => {
            let _ = reply.send(Err(Refusal::Unsupported));
            return;
        }
    };
    let path = format!("/v1/blocks/{person_id}");
    match super::asked(method, saved, &path, None) {
        Ok((200 | 204, _)) => {
            let rel = if blocked {
                protocol::Relation::Blocked
            } else {
                protocol::Relation::None
            };
            let _ = ctx.relation(person_id, rel);
            hide_direct_for_peer(ctx, person_id, blocked);
            let _ = reply.send(Ok(()));
        }
        other => answer_ok(other, reply),
    }
}

fn mark_read(saved: &Saved, conversation_id: &str, message_id: &str) {
    if !super::safe_id(message_id) {
        return;
    }
    let path = if let Some((_, root_id)) = split_thread_id(conversation_id) {
        format!("/v1/messages/{root_id}/thread/read")
    } else if super::safe_id(conversation_id) {
        format!("/v1/conversations/{conversation_id}/read")
    } else {
        return;
    };
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
    message.edited_at = wire.edited_at.as_deref().and_then(unix_of);
    message.deleted = deleted;
    message.moderated = deleted
        && wire
            .deleted_by
            .as_deref()
            .is_some_and(|by| !by.is_empty() && by != wire.author_id);
    message.pinned = !deleted && wire.pinned;
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
            if kind == MediaKind::Audio
                && file.duration > 0
                && (file.mime.eq_ignore_ascii_case("audio/ogg")
                    || file.name.to_ascii_lowercase().ends_with(".ogg")
                    || file.name.to_ascii_lowercase().ends_with(".opus"))
            {
                attachment.waveform = default_voice_waveform();
            }
            media.push((attachment.file.clone(), fetched_url(saved, file)));
            message.attachments.push(attachment);
        }
    }
    for reaction in &wire.reactions {
        if reaction.count == 0 {
            continue;
        }
        if let Some(emoji_id) = reaction.emoji_id.as_deref().filter(|id| super::safe_id(id)) {
            let file = emoji_media_for(saved, emoji_id);
            let label = reaction.name.as_deref().map(|n| format!(":{n}:")).unwrap_or_else(|| reaction.emoji.clone());
            message.reactions.push(Reaction { emoji: label, file, count: reaction.count, mine: reaction.me, who: Vec::new() });
        } else if unicode_emoji(&reaction.emoji) {
            message.reactions.push(Reaction { emoji: reaction.emoji.clone(), file: None, count: reaction.count, mine: reaction.me, who: Vec::new() });
        }
    }
    if !deleted {
        apply_mentions(&mut message, saved, &wire.mentions, false);
        apply_emoji(&mut message, saved, &wire.emoji);
        if let Some(sticker_id) = wire.sticker_id.as_deref() {
            apply_sticker(&mut message, saved, sticker_id);
        }
        apply_wire_previews(&mut message, saved, &wire.previews, media);
    }
    relink(&mut message);
    Some(message)
}

fn emoji_media_for(saved: &Saved, emoji_id: &str) -> Option<MediaFile> {
    let known = super::manage::known(saved);
    let item = known.emoji.iter().find(|e| e.id == emoji_id)?;
    let mut file = MediaFile::new(emoji_media_id(&item.id), if item.animated { MediaKind::Gif } else { MediaKind::Image });
    if !item.path.is_empty() {
        file.path = Some(item.path.clone());
    }
    Some(file)
}

fn apply_emoji(message: &mut Message, saved: &Saved, spans: &[e2ee_core::envelope::EmojiRef]) {
    if spans.is_empty() || message.text.is_empty() {
        return;
    }
    for span in spans.iter().take(50) {
        let (start, end) = (span.start as usize, span.end as usize);
        if start >= end
            || end > message.text.len()
            || !message.text.is_char_boundary(start)
            || !message.text.is_char_boundary(end)
        {
            continue;
        }
        // An emoji id no longer in the community list stays as `:name:` plain text.
        if let Some(file) = emoji_media_for(saved, &span.id) {
            message.spans.push(protocol::Span {
                start: span.start,
                end: span.end,
                style: protocol::Style::Emoji { file },
            });
        }
    }
}

fn apply_sticker(message: &mut Message, saved: &Saved, sticker_id: &str) {
    if !super::safe_id(sticker_id) {
        return;
    }
    let known = super::manage::known(saved);
    let Some(item) = known.emoji.iter().find(|e| e.id == sticker_id && e.kind == "sticker") else {
        return;
    };
    let mut file = MediaFile::new(emoji_media_id(&item.id), if item.animated { MediaKind::Gif } else { MediaKind::Image });
    if !item.path.is_empty() {
        file.path = Some(item.path.clone());
    }
    let mut attachment = Attachment::new(item.name.clone(), file);
    attachment.sticker = true;
    message.attachments.push(attachment);
}

fn apply_wire_previews(
    message: &mut Message,
    saved: &Saved,
    previews: &[WirePreview],
    media: &mut Vec<(MediaFile, String)>,
) {
    for p in previews.iter().take(e2ee_core::envelope::MAX_PREVIEWS) {
        if !(p.url.starts_with("https://") || p.url.starts_with("http://")) {
            continue;
        }
        let site = super::preview::real_host(&p.url);
        if site.is_empty() {
            continue;
        }
        let image = p
            .image_attachment_id
            .as_deref()
            .filter(|id| super::safe_id(id))
            .map(|id| {
                let file = MediaFile::new(id, MediaKind::Image);
                let url = format!("{}/v1/media/{id}", saved.base_url.trim_end_matches('/'));
                media.push((file.clone(), url));
                file
            });
        message.previews.push(protocol::Preview {
            url: p.url.clone(),
            site,
            title: super::preview::clean_field(&p.title, 200),
            description: super::preview::clean_field(&p.description, 500),
            image,
        });
    }
}

fn apply_sealed_previews(
    message: &mut Message,
    saved: &Saved,
    previews: &[e2ee::OpenedPreview],
    media: &mut Vec<(MediaFile, String)>,
) {
    for p in previews.iter().take(e2ee_core::envelope::MAX_PREVIEWS) {
        if !(p.url.starts_with("https://") || p.url.starts_with("http://")) {
            continue;
        }
        let site = super::preview::real_host(&p.url);
        if site.is_empty() {
            continue;
        }
        let image = p
            .image
            .as_ref()
            .filter(|img| super::safe_id(&img.id))
            .map(|img| {
                let mut file = MediaFile::new(&img.id, MediaKind::Image);
                file.width = img.width;
                file.height = img.height;
                file.bytes = img.bytes;
                let url = format!("{}/v1/media/{}", saved.base_url.trim_end_matches('/'), img.id);
                media.push((file.clone(), url));
                file
            });
        message.previews.push(protocol::Preview {
            url: p.url.clone(),
            site,
            title: super::preview::clean_field(&p.title, 200),
            description: super::preview::clean_field(&p.description, 500),
            image,
        });
    }
}

/// Maps structured mentions onto `Style::Mention` spans, validates UTF-8 byte ranges against
/// `message.text`, and sets `message.mentions_me` when an entry names this account, one of its
/// roles, or everyone.
fn apply_mentions(message: &mut Message, saved: &Saved, mentions: &[e2ee_core::envelope::MentionRef], _encrypted: bool) {
    if mentions.is_empty() || message.text.is_empty() {
        return;
    }
    let known = super::manage::known(saved);
    let my_roles: &[String] = known.access.as_ref().map(|a| a.roles.as_slice()).unwrap_or(&[]);
    for mention in mentions.iter().take(50) {
        let (start, end) = (mention.start as usize, mention.end as usize);
        if start >= end
            || end > message.text.len()
            || !message.text.is_char_boundary(start)
            || !message.text.is_char_boundary(end)
        {
            continue;
        }
        let (kind, id, hits_me) = match mention.kind.as_str() {
            "user" => {
                let Some(id) = mention.id.as_deref().filter(|id| !id.is_empty()) else {
                    continue;
                };
                (protocol::MentionKind::User, id.to_owned(), id == saved.user_id)
            }
            "role" => {
                let Some(id) = mention.id.as_deref().filter(|id| !id.is_empty()) else {
                    continue;
                };
                (protocol::MentionKind::Role, id.to_owned(), my_roles.iter().any(|r| r == id))
            }
            "everyone" => (protocol::MentionKind::Everyone, String::new(), true),
            _ => continue,
        };
        message.spans.push(protocol::Span {
            start: mention.start,
            end: mention.end,
            style: protocol::Style::Mention { kind, id },
        });
        if hits_me && !message.outgoing {
            message.mentions_me = true;
        }
    }
}

/// Web addresses in the text become links, preserving any structured mention or custom-emoji spans.
fn relink(message: &mut Message) {
    message.spans.retain(|span| !matches!(span.style, protocol::Style::Link { .. }));
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
    message.moderated = deleted
        && wire
            .deleted_by
            .as_deref()
            .is_some_and(|by| !by.is_empty() && by != wire.author_id);
    message.pinned = !deleted && wire.pinned;
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
                apply_mentions(&mut message, saved, &opened.mentions, true);
                apply_emoji(&mut message, saved, &opened.emoji);
                if let Some(sticker_id) = opened.sticker_id.as_deref() {
                    apply_sticker(&mut message, saved, sticker_id);
                }
                apply_sealed_previews(&mut message, saved, &opened.previews, media);
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
        message.reactions.push(Reaction { emoji: reaction.emoji.clone(), file: None, count: reaction.count, mine: reaction.me, who: Vec::new() });
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
        let kind = if file.voice || file.mime.to_ascii_lowercase().starts_with("audio/") {
            MediaKind::Audio
        } else {
            match media_kind("", &file.mime) {
                // A picture is shown as one only in a format the cache checks; anything else is a file.
                MediaKind::Image if matches!(file.mime.as_str(), "image/png" | "image/jpeg" | "image/webp") => MediaKind::Image,
                MediaKind::Gif => MediaKind::Gif,
                _ => MediaKind::File,
            }
        };
        let mut stored = MediaFile::new(&file.id, kind);
        stored.width = file.width;
        stored.height = file.height;
        stored.bytes = file.bytes;
        let mut attachment = Attachment::new(title_or(&file.name, "file"), stored);
        attachment.mime = file.mime.clone();
        attachment.duration = file.duration;
        if file.voice {
            attachment.waveform = if file.waveform.is_empty() { default_voice_waveform() } else { file.waveform.clone() };
        }
        media.push((attachment.file.clone(), format!("{}/v1/media/{}", saved.base_url.trim_end_matches('/'), file.id)));
        out.push(attachment);
    }
    out
}

/// Subtle fallback waveform for plaintext Tiny voice messages (the server stores `duration`
/// without a waveform array).
fn default_voice_waveform() -> Vec<u8> {
    vec![64, 96, 128, 160, 112, 144, 176, 128, 96, 144, 160, 112, 80, 128, 160, 144, 112, 96, 128, 96, 80, 64, 80, 64]
}

fn media_kind(kind: &str, mime: &str) -> MediaKind {
    let mime_lower = mime.to_ascii_lowercase();
    match kind {
        "image" => MediaKind::Image,
        "gif" => MediaKind::Gif,
        "video" => MediaKind::Video,
        "file" if mime_lower.starts_with("audio/") => MediaKind::Audio,
        "file" => MediaKind::File,
        _ => {
            if mime_lower == "image/gif" {
                MediaKind::Gif
            } else if mime_lower.starts_with("video/") {
                MediaKind::Video
            } else if mime_lower.starts_with("audio/") {
                MediaKind::Audio
            } else if mime_lower.starts_with("image/") {
                MediaKind::Image
            } else {
                MediaKind::File
            }
        }
    }
}

/// Unicode emoji only. A shortcode, a word, or a path is not one. A keycap is one, though it
/// starts with an ASCII digit (1️⃣ is `1`, U+FE0F, U+20E3), and tinyserver takes it.
pub(crate) fn unicode_emoji(emoji: &str) -> bool {
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
    let server_caps = super::manage::capabilities_of(saved);
    let scans = server_caps.has(protocol::TINY_FEATURE_FILE_SCAN);
    let max_file_bytes = limits.image.max(limits.file);
    let presence_status = server_caps.has(protocol::TINY_FEATURE_PRESENCE_STATUS);
    Capabilities {
        max_file_bytes,
        max_files: protocol::ATTACHMENTS_LIMIT as u32,
        direct_calls: directs_callable(saved),
        file_scan: scans,
        allow_archives: scans && limits.archives,
        abuse_reports: server_caps.has(protocol::TINY_FEATURE_ABUSE_REPORTS),
        voice_notes: max_file_bytes > 0,
        mention_spans: server_caps.has(protocol::TINY_FEATURE_MENTIONS),
        custom_emoji: if server_caps.has(protocol::TINY_FEATURE_CUSTOM_EMOJI) {
            protocol::EmojiScope::Space
        } else {
            protocol::EmojiScope::None
        },
        stickers: server_caps.has(protocol::TINY_FEATURE_STICKERS),
        suppress_embeds: server_caps.has(protocol::TINY_FEATURE_LINK_PREVIEWS),
        threads: server_caps.has(protocol::TINY_FEATURE_THREADS),
        reply: true,
        edit: true,
        delete: true,
        pins: server_caps.has(protocol::TINY_FEATURE_PINS),
        search: server_caps.has(protocol::TINY_FEATURE_MESSAGE_SEARCH),
        blocks: server_caps.has(protocol::TINY_FEATURE_BLOCKS),
        reactions: true,
        gif_file: true,
        presence_status,
        profile_avatar: true,
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

fn roles_of(saved: &Saved) -> Vec<protocol::TinyRole> {
    optional_list::<protocol::TinyRole>(saved, "/v1/roles", "roles").unwrap_or_default()
}

#[derive(Clone)]
struct CachedDirectory {
    fetched: std::time::Instant,
    members: Vec<protocol::TinyMember>,
    roles: Vec<protocol::TinyRole>,
    overrides: HashMap<String, Vec<protocol::TinyOverride>>,
}

const DIRECTORY_TTL: std::time::Duration = std::time::Duration::from_secs(30);

fn directory_cache() -> &'static std::sync::Mutex<HashMap<String, CachedDirectory>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, CachedDirectory>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn directory(saved: &Saved) -> CachedDirectory {
    let key = format!("{}|{}", saved.base_url, saved.user_id);
    if let Ok(cache) = directory_cache().lock() {
        if let Some(entry) = cache.get(&key) {
            if entry.fetched.elapsed() < DIRECTORY_TTL {
                return entry.clone();
            }
        }
    }
    let members = super::members_of(saved).unwrap_or_default();
    let roles = roles_of(saved);
    let overrides = optional_list::<WireChannel>(saved, "/v1/channels", "channels")
        .unwrap_or_default()
        .into_iter()
        .map(|ch| (ch.id, ch.overrides))
        .collect();
    let entry = CachedDirectory {
        fetched: std::time::Instant::now(),
        members,
        roles,
        overrides,
    };
    if let Ok(mut cache) = directory_cache().lock() {
        cache.insert(key, entry.clone());
    }
    entry
}

fn member_display_name(member: &protocol::TinyMember) -> String {
    let trimmed = member.display_name.trim();
    if trimmed.is_empty() {
        "Someone".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn channel_members(
    ctx: &Context,
    saved: &Saved,
    conversation_id: &str,
    offset: u32,
    limit: u32,
    reply: crate::adapters::Reply<Members>,
) {
    let dir = directory(saved);
    let overrides = dir
        .overrides
        .get(conversation_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut visible: Vec<&protocol::TinyMember> = dir
        .members
        .iter()
        .filter(|member| {
            let is_owner = member.owner || member.role.eq_ignore_ascii_case("owner");
            if dir.roles.is_empty() {
                true
            } else {
                protocol::tiny_room_permissions(&dir.roles, &member.roles, is_owner, overrides)
                    & protocol::TINY_VIEW_CHANNEL
                    != 0
            }
        })
        .collect();
    visible.sort_by_key(|member| member_display_name(member).to_lowercase());

    let mut hoisted: Vec<&protocol::TinyRole> = dir
        .roles
        .iter()
        .filter(|role| role.hoist && !role.default)
        .collect();
    hoisted.sort_by_key(|role| std::cmp::Reverse(role.position));

    let mut owners = Vec::new();
    let mut by_role: Vec<Vec<&protocol::TinyMember>> = vec![Vec::new(); hoisted.len()];
    let mut rest = Vec::new();
    for member in visible {
        if member.owner || member.role.eq_ignore_ascii_case("owner") {
            owners.push(member);
        } else if let Some(at) = hoisted.iter().position(|role| member.roles.contains(&role.id)) {
            by_role[at].push(member);
        } else {
            rest.push(member);
        }
    }

    let mut sections = Vec::new();
    let mut ordered = Vec::new();
    let mut push_group = |name: String, group: Vec<&protocol::TinyMember>| {
        if !group.is_empty() {
            sections.push(protocol::MemberSection {
                name,
                count: group.len() as u32,
            });
            for member in group {
                ordered.push(protocol::Person::new(
                    ctx.key(&member.user_id),
                    member_display_name(member),
                ));
            }
        }
    };
    push_group("Owner".to_owned(), owners);
    for (role, group) in hoisted.iter().zip(by_role) {
        push_group(role.name.clone(), group);
    }
    push_group("Members".to_owned(), rest);

    let members = ordered
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    let _ = reply.send(Ok(Members {
        pending: false,
        sections,
        members,
    }));
}

fn conversation_info(
    ctx: &Context,
    saved: &Saved,
    conversation_id: &str,
    reply: crate::adapters::Reply<ConversationAbout>,
) {
    let mut about = ConversationAbout::default();
    if let Ok(Some(row)) = ctx.known(conversation_id) {
        about.description = row.topic;
        if row.kind == ConversationKind::Channel {
            about.members = super::members_of(saved).ok().map(|members| members.len() as u32);
        }
    }
    let _ = reply.send(Ok(about));
}

fn profile(saved: &Saved, person_id: &str, reply: crate::adapters::Reply<About>) {
    let mut about = About::default();
    if super::safe_id(person_id) {
        if let Ok(members) = super::members_of(saved) {
            if let Some(member) = members.into_iter().find(|member| member.user_id == person_id) {
                about.joined = rfc3339(&member.created_at);
                about.last_seen = member.last_seen_at.as_deref().and_then(rfc3339);
                let roles = roles_of(saved);
                let mut named: Vec<&protocol::TinyRole> = roles
                    .iter()
                    .filter(|role| !role.default && member.roles.contains(&role.id))
                    .collect();
                named.sort_by_key(|role| std::cmp::Reverse(role.position));
                about.roles = named.into_iter().map(|role| role.name.clone()).collect();
                if about.roles.is_empty() && (member.owner || member.role.eq_ignore_ascii_case("owner")) {
                    about.roles.push("Owner".to_owned());
                }
            }
        }
    }
    let _ = reply.send(Ok(about));
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

/// Probe stored conversations when their list came back empty (capped at 50 probes). A 404
/// confirms the channel/direct is gone or no longer viewable and removes it; 200, any other
/// status, or a transport error keeps the row and its stored messages.
const EMPTY_LIST_PROBE_CAP: usize = 50;

fn probe_survivors(ctx: &Context, saved: &Saved, stored: Vec<ConversationRow>) -> Vec<ConversationRow> {
    let mut survivors = Vec::new();
    for (index, row) in stored.into_iter().enumerate() {
        if index >= EMPTY_LIST_PROBE_CAP || !super::safe_id(&row.id) {
            survivors.push(row);
            continue;
        }
        let path = format!("/v1/conversations/{}/messages?limit=1", row.id);
        match super::asked("GET", saved, &path, None) {
            Ok((404, _)) => {
                let _ = ctx.conversation_gone(&row.id);
            }
            _ => survivors.push(row),
        }
    }
    survivors
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
    if id.is_empty() { None } else { Some(ReplyTo { id, sender: String::new(), snippet: String::new(), person: None, deleted: false, thumb: None, story: false }) }
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
    position: i32,
    #[serde(default, deserialize_with = "flex_u32")]
    unread: u32,
    #[serde(default, deserialize_with = "flex_u32")]
    mentions: u32,
    #[serde(default)]
    last_message: Option<WireLast>,
    /// End-to-end encrypted, for good once it is. Absent on a server older than encrypted text.
    #[serde(default)]
    e2ee: bool,
    /// Encrypted now, and some message from before that is still stored in the clear.
    #[serde(default)]
    plaintext_history: bool,
    /// Per-role permission overrides on this channel, when the server includes them.
    #[serde(default)]
    overrides: Vec<protocol::TinyOverride>,
    /// Caller's effective permission bits in this channel, when the server includes them.
    #[serde(default)]
    my_permissions: Option<u64>,
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
    #[serde(default, deserialize_with = "flex_u32")]
    mentions: u32,
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
    duration_s: Option<u32>,
    #[serde(default)]
    ended_at: String,
}

fn format_call_duration(seconds: u32) -> String {
    if seconds >= 3600 {
        let h = seconds / 3600;
        let m = (seconds % 3600) / 60;
        if m == 0 { format!("{h} hr") } else { format!("{h} hr {m} min") }
    } else if seconds >= 60 {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} sec")
    }
}

/// The transcript line for a finished call. Nothing about the conversation row changes except the
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
    let outcome = match call.outcome.as_str() {
        "missed" => protocol::CallOutcome::Missed,
        "declined" => protocol::CallOutcome::Declined,
        "cancelled" => protocol::CallOutcome::Cancelled,
        "ended" => protocol::CallOutcome::Ended,
        _ => return,
    };
    let Some(row) = ctx.known(direct_id).ok().flatten() else {
        return;
    };
    let outgoing = call.caller_id == saved.user_id;
    let name = caller_name(ctx, saved, names, direct_id, &call.caller_id);
    let other = if outgoing {
        if !row.title.is_empty() {
            row.title.clone()
        } else {
            row.peer
                .as_deref()
                .and_then(|peer| names.get(peer))
                .filter(|n| !n.is_empty())
                .cloned()
                .unwrap_or_else(|| "Someone".to_owned())
        }
    } else {
        name.clone()
    };
    let seconds = call.duration_s.filter(|&s| s > 0);
    let text = match (outcome, outgoing) {
        (protocol::CallOutcome::Missed, false) => format!("Missed call from {other}"),
        (protocol::CallOutcome::Missed, true) => format!("You called {other} · no answer"),
        (protocol::CallOutcome::Declined, false) => format!("You declined a call from {other}"),
        (protocol::CallOutcome::Declined, true) => format!("{other} declined the call"),
        (protocol::CallOutcome::Cancelled, false) => format!("{other} cancelled the call"),
        (protocol::CallOutcome::Cancelled, true) => "You cancelled the call".to_owned(),
        (protocol::CallOutcome::Busy, _) => format!("{other}'s line was busy"),
        (protocol::CallOutcome::Ended, _) => match seconds {
            Some(secs) => format!("Call with {other} · {}", format_call_duration(secs)),
            None => format!("Call with {other}"),
        },
        (protocol::CallOutcome::Live, _) => "Call in progress".to_owned(),
    };
    let mut message = Message::plain(
        ring_id,
        name,
        text,
        rfc3339(&call.ended_at).unwrap_or(0),
        outgoing,
    );
    message.kind = MessageKind::System;
    message.sender.id = call.caller_id.clone();
    message.call = Some(protocol::CallNote {
        outcome,
        caller_id: call.caller_id.clone(),
        seconds,
        participants: Vec::new(),
        video: false,
    });
    let _ = ctx.quiet_line(direct_id, &message, outcome == protocol::CallOutcome::Missed && !outgoing);
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
struct WireThreadSummary {
    #[serde(default)]
    replies: u32,
    #[serde(default)]
    last_reply_at: Option<String>,
    #[serde(default)]
    participants: Vec<String>,
    #[serde(default)]
    unread: u32,
}

#[derive(Deserialize)]
struct WireFollowedThread {
    channel_id: String,
    message_id: String,
    #[serde(default)]
    message: Option<WireMessage>,
    #[serde(default)]
    thread: Option<WireThreadSummary>,
}

#[derive(Deserialize)]
struct WirePreview {
    url: String,
    #[serde(default)]
    #[allow(dead_code)]
    site: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    image_attachment_id: Option<String>,
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
    thread_id: Option<String>,
    #[serde(default)]
    thread: Option<WireThreadSummary>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    edited_at: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
    #[serde(default)]
    deleted_by: Option<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    attachments: Vec<WireFile>,
    #[serde(default)]
    reactions: Vec<WireReaction>,
    #[serde(default)]
    mentions: Vec<e2ee_core::envelope::MentionRef>,
    #[serde(default)]
    emoji: Vec<e2ee_core::envelope::EmojiRef>,
    #[serde(default)]
    sticker_id: Option<String>,
    #[serde(default)]
    previews: Vec<WirePreview>,
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
    #[serde(default)]
    emoji: String,
    #[serde(default)]
    emoji_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
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
                r#"{{"channels":[{{"id":"channel-1","name":"general","topic":"Hello","category_id":"cat-1","position":1,"unread":2,"last_message":{{"id":"c-9","author_id":"{PEER}","body":"hey","created_at":"{WHEN}"}}}}]}}"#
            ));
        }
        if request.starts_with("GET /v1/categories ") {
            return http(200, "OK", r#"[{"id":"cat-1","name":"Text","position":0}]"#);
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
        assert_eq!(post_message(&ctx, &saved, "first-call", "client-call", "secret", None, &[], &[], None).unwrap_err(), NOT_SET_UP, "text enrollment is still required");
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

    #[test]
    fn store_groups_use_category_names_and_positions_and_rename_on_category_updated() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/categories ") {
                return Some(http(200, "OK", r#"[{"id":"cat-text","name":"Text","position":2},{"id":"cat-proj","name":"Projects","position":5}]"#));
            }
            if request.starts_with("GET /v1/channels ") {
                return Some(http(200, "OK", r#"{"channels":[{"id":"c-1","name":"general","topic":"","category_id":"cat-text","position":10,"unread":0},{"id":"c-2","name":"random","topic":"","category_id":"cat-text","position":11,"unread":0},{"id":"c-3","name":"roadmap","topic":"","category_id":"cat-proj","position":0,"unread":0}]}"#));
            }
            None
        });
        let space = ctx.key(SPACE);
        let groups = ctx.hub.store().tree(&space, 0, 50).unwrap().unwrap().groups;
        let text = groups.iter().find(|g| g.id == "cat-text").expect("Text group");
        let proj = groups.iter().find(|g| g.id == "cat-proj").expect("Projects group");
        assert_eq!((text.name.as_str(), text.position), ("Text", 2));
        assert_eq!((proj.name.as_str(), proj.position), ("Projects", 5));

        on_live(&ctx, &saved, &HashMap::new(), "category.updated", &json!({
            "category": { "id": "cat-proj", "name": "Roadmap", "position": 1 }
        }));
        let groups = ctx.hub.store().tree(&space, 0, 50).unwrap().unwrap().groups;
        let proj = groups.iter().find(|g| g.id == "cat-proj").expect("renamed group");
        assert_eq!((proj.name.as_str(), proj.position), ("Roadmap", 1));
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
            if request.starts_with("GET /v1/conversations/channel-1/messages?limit=1 ") {
                return Some(http(200, "OK", r#"{"messages":[]}"#));
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

    #[test]
    fn an_empty_channel_list_removes_a_channel_confirmed_gone_by_404_and_refreshes_the_rest() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/channels ") && seen.fetch_add(1, Ordering::Relaxed) >= 1 {
                return Some(http(200, "OK", r#"{"channels":[]}"#));
            }
            if request.starts_with("GET /v1/conversations/channel-1/messages?limit=1 ") {
                return Some(http(404, "Not Found", r#"{"error":"not_found"}"#));
            }
            if request.starts_with("GET /v1/directs ") && seen.load(Ordering::Relaxed) >= 2 {
                return Some(http(
                    200,
                    "OK",
                    &format!(
                        r#"{{"directs":[{{"id":"direct-1","peer":{{"user_id":"{PEER}","display_name":"Bea"}}}},{{"id":"direct-2","peer":{{"user_id":"{PEER}","display_name":"Bea New"}}}}]}}"#
                    ),
                ));
            }
            None
        });
        let kept = Message::plain("m-kept", "Ada", "kept", 1_700_000_000, false);
        ctx.hub.store().ingest(&ctx.key("channel-1"), &kept, false).unwrap();
        refresh(&ctx, &saved);
        assert!(ctx.known("channel-1").unwrap().is_none(), "404-confirmed deleted channel must be removed");
        assert!(ctx.known("direct-2").unwrap().is_some(), "directs must still refresh when channels is empty");
    }

    #[test]
    fn an_empty_channel_list_keeps_a_channel_confirmed_alive_by_200_or_error_and_refreshes_the_rest() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/channels ") && seen.fetch_add(1, Ordering::Relaxed) >= 1 {
                return Some(http(200, "OK", r#"{"channels":[]}"#));
            }
            if request.starts_with("GET /v1/conversations/channel-1/messages?limit=1 ") {
                return Some(http(500, "Internal Server Error", r#"{"error":"boom"}"#));
            }
            if request.starts_with("GET /v1/directs ") && seen.load(Ordering::Relaxed) >= 2 {
                return Some(http(
                    200,
                    "OK",
                    &format!(
                        r#"{{"directs":[{{"id":"direct-1","peer":{{"user_id":"{PEER}","display_name":"Bea"}}}},{{"id":"direct-2","peer":{{"user_id":"{PEER}","display_name":"Bea New"}}}}]}}"#
                    ),
                ));
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
            .expect("probe error keeps the channel");
        assert_eq!(
            page.messages.iter().find(|message| message.id == "m-kept").map(|message| message.text.as_str()),
            Some("kept")
        );
        assert!(ctx.known("direct-2").unwrap().is_some(), "directs must still refresh when channels probe keeps survivors");
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
            if request.starts_with("GET /v1/conversations/direct-1/messages?limit=1 ") {
                return Some(http(200, "OK", r#"{"messages":[]}"#));
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
    fn conversation_info_and_profile_answer_without_refusal_or_email() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/members ") {
                return Some(http(
                    200,
                    "OK",
                    &format!(
                        r#"[{{"user_id":"{PEER}","display_name":"Bea","email":"bea@secret.test","role":"member","roles":["r-mod"],"created_at":"{WHEN}"}}]"#
                    ),
                ));
            }
            if request.starts_with("GET /v1/roles ") {
                return Some(http(
                    200,
                    "OK",
                    r#"[{"id":"r-mod","name":"Mods","position":2,"permissions":1,"hoist":true,"default":false}]"#,
                ));
            }
            None
        });
        let live = Live::new().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &live, Command::ConversationInfo { conversation_id: "channel-1".into(), reply: tx });
        let info = rx.recv().unwrap().expect("ConversationInfo must answer Ok");
        assert_eq!(info.members, Some(1));

        let (tx, rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &live, Command::Profile { person_id: PEER.into(), reply: tx });
        let about = rx.recv().unwrap().expect("Profile must answer Ok");
        assert_eq!(about.roles, vec!["Mods".to_owned()]);
        assert_eq!(about.joined, rfc3339(WHEN));
        assert!(about.contacts.is_empty() && about.fields.is_empty(), "never expose email");
    }

    #[test]
    fn members_filters_by_view_channel_groups_sections_and_omits_email() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let member_hits = Arc::new(AtomicUsize::new(0));
        let hits = member_hits.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            if request.starts_with("GET /v1/channels ") {
                return Some(http(
                    200,
                    "OK",
                    r#"{"channels":[{"id":"channel-1","name":"secret","position":0,"overrides":[{"role_id":"r-everyone","allow":0,"deny":1},{"role_id":"r-mod","allow":1,"deny":0}]}]}"#,
                ));
            }
            if request.starts_with("GET /v1/members ") {
                hits.fetch_add(1, Ordering::Relaxed);
                return Some(http(
                    200,
                    "OK",
                    &format!(
                        r#"[
                            {{"user_id":"{PEER}","display_name":"Bea","email":"bea@secret.test","role":"member","roles":["r-mod"]}},
                            {{"user_id":"{ME}","display_name":"","email":"hidden@secret.test","role":"member","roles":[]}}
                        ]"#
                    ),
                ));
            }
            if request.starts_with("GET /v1/roles ") {
                return Some(http(
                    200,
                    "OK",
                    r#"[
                        {"id":"r-everyone","name":"@everyone","position":0,"permissions":1,"hoist":false,"default":true},
                        {"id":"r-mod","name":"Mods","position":2,"permissions":1,"hoist":true,"default":false}
                    ]"#,
                ));
            }
            None
        });
        let live = Live::new().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &live, Command::Members { conversation_id: "channel-1".into(), offset: 0, limit: 50, reply: tx });
        let page = rx.recv().unwrap().expect("Command::Members must answer Ok");
        assert!(!page.pending);
        assert_eq!(page.members.len(), 1, "denied member must be excluded");
        assert_eq!(page.members[0].key.id, PEER);
        assert_eq!(page.members[0].name, "Bea");
        assert!(page.members[0].handle.is_empty(), "never include email");
        assert_eq!(page.sections, vec![protocol::MemberSection { name: "Mods".into(), count: 1 }]);

        // A second click with offset=1 reuses the cached directory and returns empty page with same sections.
        let before = member_hits.load(Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &live, Command::Members { conversation_id: "channel-1".into(), offset: 1, limit: 50, reply: tx });
        let page2 = rx.recv().unwrap().expect("second Command::Members must answer Ok");
        assert!(page2.members.is_empty(), "offset=1 skips the single member");
        assert_eq!(page2.sections, vec![protocol::MemberSection { name: "Mods".into(), count: 1 }]);
        assert_eq!(member_hits.load(Ordering::Relaxed), before, "recent directory copy is reused without another GET /v1/members");
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
        assert_eq!(stored.text, "Missed call from Bea");
        assert_eq!(stored.kind, MessageKind::System);
        assert_eq!(stored.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Missed));
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
        assert_eq!(mine.text, "Call with Bea");
        assert_eq!(mine.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Ended));
        assert!(mine.outgoing);
        assert_eq!(mine.kind, MessageKind::System);
        let after = ctx.known("direct-1").unwrap().unwrap();
        assert_eq!(after.preview, "mine");
        assert_eq!(after.unread, 2);
    }

    #[test]
    fn record_call_sets_outcome_wording_and_replaces_ring_in_place() {
        let (_adapter, ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/capabilities ") {
                return Some(http(200, "OK", r#"{"features":["direct_calls"]}"#));
            }
            if request.starts_with("GET /v1/directs ") {
                return Some(http(
                    200,
                    "OK",
                    &format!(
                        r#"{{"directs":[{{"id":"direct-1","peer":{{"user_id":"{PEER}","display_name":"Bea"}},"unread":0,"last_read":"m-1","last_message":{{"id":"m-2","author_id":"{ME}","body":"hi","created_at":"{WHEN}"}}}}]}}"#
                    ),
                ));
            }
            None
        });
        let names = HashMap::from([(PEER.to_owned(), "Bea".to_owned())]);

        // 1. Incoming missed
        on_live(&ctx, &saved, &names, "direct.call", &json!({
            "direct_id": "direct-1", "ring_id": "r-miss-in", "caller_id": PEER, "outcome": "missed", "ended_at": WHEN
        }));
        let m = ctx.stored_message("direct-1", "r-miss-in").unwrap().unwrap();
        assert_eq!(m.call, Some(protocol::CallNote { outcome: protocol::CallOutcome::Missed, caller_id: PEER.into(), seconds: None, participants: Vec::new(), video: false }));
        assert_eq!(m.text, "Missed call from Bea");
        assert_eq!(ctx.known("direct-1").unwrap().unwrap().unread, 1);

        // 2. Outgoing missed (no answer)
        on_live(&ctx, &saved, &names, "direct.call", &json!({
            "direct_id": "direct-1", "ring_id": "r-miss-out", "caller_id": ME, "outcome": "missed", "ended_at": WHEN
        }));
        let m = ctx.stored_message("direct-1", "r-miss-out").unwrap().unwrap();
        assert_eq!(m.call, Some(protocol::CallNote { outcome: protocol::CallOutcome::Missed, caller_id: ME.into(), seconds: None, participants: Vec::new(), video: false }));
        assert_eq!(m.text, "You called Bea · no answer");

        // 3. Incoming declined
        on_live(&ctx, &saved, &names, "direct.call", &json!({
            "direct_id": "direct-1", "ring_id": "r-dec-in", "caller_id": PEER, "outcome": "declined", "ended_at": WHEN
        }));
        let m = ctx.stored_message("direct-1", "r-dec-in").unwrap().unwrap();
        assert_eq!(m.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Declined));
        assert_eq!(m.text, "You declined a call from Bea");

        // 4. Outgoing declined
        on_live(&ctx, &saved, &names, "direct.call", &json!({
            "direct_id": "direct-1", "ring_id": "r-dec-out", "caller_id": ME, "outcome": "declined", "ended_at": WHEN
        }));
        let m = ctx.stored_message("direct-1", "r-dec-out").unwrap().unwrap();
        assert_eq!(m.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Declined));
        assert_eq!(m.text, "Bea declined the call");

        // 5. Cancelled (both directions)
        on_live(&ctx, &saved, &names, "direct.call", &json!({
            "direct_id": "direct-1", "ring_id": "r-can-in", "caller_id": PEER, "outcome": "cancelled", "ended_at": WHEN
        }));
        assert_eq!(ctx.stored_message("direct-1", "r-can-in").unwrap().unwrap().text, "Bea cancelled the call");
        on_live(&ctx, &saved, &names, "direct.call", &json!({
            "direct_id": "direct-1", "ring_id": "r-can-out", "caller_id": ME, "outcome": "cancelled", "ended_at": WHEN
        }));
        assert_eq!(ctx.stored_message("direct-1", "r-can-out").unwrap().unwrap().text, "You cancelled the call");

        // 6. Ended with duration, and replacing same ring_id in place without double-bumping unread
        on_live(&ctx, &saved, &names, "direct.call", &json!({
            "direct_id": "direct-1", "ring_id": "r-miss-in", "caller_id": PEER, "outcome": "ended", "duration_s": 720, "ended_at": WHEN
        }));
        let replaced = ctx.stored_message("direct-1", "r-miss-in").unwrap().unwrap();
        assert_eq!(replaced.call, Some(protocol::CallNote { outcome: protocol::CallOutcome::Ended, caller_id: PEER.into(), seconds: Some(720), participants: Vec::new(), video: false }));
        assert_eq!(replaced.text, "Call with Bea · 12 min");
        assert_eq!(ctx.known("direct-1").unwrap().unwrap().unread, 1);
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
        let outcome = post_message(&ctx, &saved, "sealed-1", "client-9", "secret words", None, &[], &[], None);
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
        let outcome = post_message(&ctx, &saved, "direct-1", "client-1", "hi", None, &[], &[], None);
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
        edit(&ctx, &saved, "direct-1", "e-1", "changed", &[], tx);
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
                view_once: false,
                media: None,
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

    #[test]
    fn voice_notes_capability_and_plaintext_and_sealed_receive() {
        let (_adapter, _ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/server ") {
                return Some(http(200, "OK", r#"{"name":"Tiny","image_max_bytes":8388608,"file_max_bytes":26214400}"#));
            }
            if request.starts_with("GET /v1/capabilities") {
                return Some(http(200, "OK", r#"{"version":"0.6.0","features":["file_scan"]}"#));
            }
            None
        });
        assert!(account_caps(&saved).voice_notes, "voice_notes enabled when uploads are allowed");

        // Plaintext voice attachment receives MediaKind::Audio, duration, and non-empty waveform.
        let wire: WireMessage = serde_json::from_value(json!({
            "id": "m-voice",
            "author_id": ME,
            "body": "",
            "created_at": WHEN,
            "attachments": [{
                "id": "file-v1",
                "name": "voice.ogg",
                "mime": "audio/ogg",
                "kind": "file",
                "bytes": 4096,
                "duration": 9
            }]
        }))
        .unwrap();
        let mut media = Vec::new();
        let msg = map_message(&saved, &HashMap::new(), &wire, &HashMap::new(), &mut media).unwrap();
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].file.kind, MediaKind::Audio);
        assert_eq!(msg.attachments[0].duration, 9);
        assert!(!msg.attachments[0].waveform.is_empty());

        // Sealed voice attachment preserves exact waveform and duration from envelope.
        let opened = e2ee::Opened {
            text: String::new(),
            attachments: vec![e2ee::OpenedFile {
                id: "file-v2".into(),
                key: "a".into(),
                name: "voice.ogg".into(),
                mime: "audio/ogg".into(),
                bytes: 2048,
                sha256: "b".into(),
                width: 0,
                height: 0,
                duration: 14,
                voice: true,
                waveform: vec![20, 120, 240, 80],
            }],
            ..e2ee::Opened::default()
        };
        let mut sealed_media = Vec::new();
        let atts = sealed_attachments(&saved, &opened, &mut sealed_media);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].file.kind, MediaKind::Audio);
        assert_eq!(atts[0].duration, 14);
        assert_eq!(atts[0].waveform, vec![20, 120, 240, 80]);
    }

    #[test]
    fn mentions_send_receive_edit_and_sealed_badge() {
        use std::sync::{Arc, Mutex};
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let log_srv = log.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            log_srv.lock().unwrap().push(request.to_owned());
            if request.starts_with("GET /v1/capabilities") {
                return Some(http(200, "OK", r#"{"version":"0.6.0","features":["mentions"]}"#));
            }
            if request.starts_with("GET /v1/me ") {
                return Some(http(200, "OK", r#"{"user_id":"11111111-1111-1111-1111-111111111111","owner":false,"roles":["role-dev"],"permissions":63,"rank":1}"#));
            }
            if request.starts_with("GET /v1/server ") {
                return Some(http(200, "OK", r#"{"name":"Tiny"}"#));
            }
            if request.starts_with("POST /v1/conversations/chan-1/messages ") {
                return Some(http(201, "Created", &json!({
                    "id": "m-10",
                    "author_id": ME,
                    "body": "hi @Bea",
                    "created_at": WHEN,
                    "mentions": [{"kind": "user", "id": PEER, "start": 3, "end": 7}]
                }).to_string()));
            }
            if request.starts_with("PATCH /v1/messages/m-10 ") {
                return Some(http(200, "OK", &json!({
                    "id": "m-10",
                    "author_id": ME,
                    "body": "hey @everyone",
                    "created_at": WHEN,
                    "edited_at": WHEN,
                    "mentions": [{"kind": "everyone", "start": 4, "end": 13}]
                }).to_string()));
            }
            None
        });
        assert!(account_caps(&saved).mention_spans);
        let spans = vec![protocol::Span {
            start: 3,
            end: 7,
            style: protocol::Style::Mention { kind: protocol::MentionKind::User, id: PEER.to_owned() },
        }];
        let sent = post_message(&ctx, &saved, "chan-1", "c-1", "hi @Bea", None, &[], &spans, None).unwrap();
        assert_eq!(sent.spans.len(), 1);
        assert!(matches!(&sent.spans[0].style, protocol::Style::Mention { kind: protocol::MentionKind::User, id } if id == PEER));
        assert!(!sent.mentions_me, "own message does not set mentions_me");

        // Verify POST body carried structured mentions.
        let post_req = log.lock().unwrap().iter().find(|r| r.starts_with("POST /v1/conversations/chan-1/messages ")).cloned().unwrap();
        let body: Value = serde_json::from_str(post_req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["mentions"][0]["kind"], "user");
        assert_eq!(body["mentions"][0]["id"], PEER);
        assert_eq!(body["mentions"][0]["start"], 3);
        assert_eq!(body["mentions"][0]["end"], 7);

        // Incoming plaintext mention of ME sets mentions_me and Style::Mention.
        let wire: WireMessage = serde_json::from_value(json!({
            "id": "m-11",
            "author_id": PEER,
            "body": "ping @Ada https://example.com",
            "created_at": WHEN,
            "mentions": [{"kind": "user", "id": ME, "start": 5, "end": 9}]
        })).unwrap();
        let mut media = Vec::new();
        let incoming = map_message(&saved, &HashMap::new(), &wire, &HashMap::new(), &mut media).unwrap();
        assert!(incoming.mentions_me);
        assert!(incoming.spans.iter().any(|s| matches!(&s.style, protocol::Style::Mention { kind: protocol::MentionKind::User, id } if id == ME)));
        assert!(incoming.spans.iter().any(|s| matches!(&s.style, protocol::Style::Link { .. })), "relink preserves mentions alongside links");

        // Edit sends updated mentions.
        let (tx, rx) = std::sync::mpsc::channel();
        let edit_spans = vec![protocol::Span {
            start: 4,
            end: 13,
            style: protocol::Style::Mention { kind: protocol::MentionKind::Everyone, id: String::new() },
        }];
        edit(&ctx, &saved, "chan-1", "m-10", "hey @everyone", &edit_spans, tx);
        assert!(rx.recv().unwrap().is_ok());
        let patch_req = log.lock().unwrap().iter().find(|r| r.starts_with("PATCH /v1/messages/m-10 ")).cloned().unwrap();
        let patch_body: Value = serde_json::from_str(patch_req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(patch_body["mentions"][0]["kind"], "everyone");

        // Sealed incoming mention of ME sets mentions_me and Style::Mention.
        let sealed_wire: WireMessage = serde_json::from_value(json!({
            "id": "m-12",
            "author_id": PEER,
            "e2ee": true,
            "created_at": WHEN
        })).unwrap();
        let mut opened = HashMap::new();
        opened.insert("m-12".to_owned(), Read::Opened(e2ee::Opened {
            text: "secret @Ada".into(),
            mentions: vec![e2ee_core::envelope::MentionRef { kind: "user".into(), id: Some(ME.to_owned()), start: 7, end: 11 }],
            ..e2ee::Opened::default()
        }));
        let sealed_msg = map_message(&saved, &HashMap::new(), &sealed_wire, &opened, &mut media).unwrap();
        assert!(sealed_msg.mentions_me);
        assert!(sealed_msg.spans.iter().any(|s| matches!(&s.style, protocol::Style::Mention { kind: protocol::MentionKind::User, id } if id == ME)));
    }

    #[test]
    fn custom_emoji_and_stickers_catalog_send_receive_and_reactions() {
        use std::sync::{Arc, Mutex};
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let log_srv = log.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            log_srv.lock().unwrap().push(request.to_owned());
            if request.starts_with("GET /v1/capabilities") {
                return Some(http(200, "OK", r#"{"version":"0.6.0","features":["custom_emoji","stickers"]}"#));
            }
            if request.starts_with("GET /v1/me ") {
                return Some(http(200, "OK", r#"{"user_id":"11111111-1111-1111-1111-111111111111","owner":true,"roles":[],"permissions":8388607,"rank":2147483647}"#));
            }
            if request.starts_with("GET /v1/server ") {
                return Some(http(200, "OK", r#"{"name":"Tiny"}"#));
            }
            if request.starts_with("GET /v1/emoji ") {
                return Some(http(200, "OK", r#"[{"id":"e-1","kind":"emoji","name":"party","animated":false,"sha256":"aabb"},{"id":"s-1","kind":"sticker","name":"wave","animated":false,"sha256":"ccdd"}]"#));
            }
            if request.starts_with("GET /v1/emoji/e-1 ") || request.starts_with("GET /v1/emoji/s-1 ") {
                let mut resp = b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 8\r\nConnection: close\r\n\r\n".to_vec();
                resp.extend_from_slice(b"\x89PNG\r\n\x1a\n");
                return Some(resp);
            }
            if request.starts_with("POST /v1/conversations/chan-1/messages ") {
                return Some(http(201, "Created", &json!({
                    "id": "m-20",
                    "author_id": ME,
                    "body": "yay :party:",
                    "created_at": WHEN,
                    "emoji": [{"id": "e-1", "start": 4, "end": 11}],
                    "sticker_id": "s-1",
                    "reactions": [{"emoji": ":party:", "emoji_id": "e-1", "name": "party", "count": 1, "me": true}]
                }).to_string()));
            }
            if request.starts_with("PUT /v1/messages/m-20/reactions ") {
                return Some(http(200, "OK", r#"{"emoji":":party:","emoji_id":"e-1","name":"party","count":1,"me":true}"#));
            }
            None
        });
        let caps = account_caps(&saved);
        assert_eq!(caps.custom_emoji, protocol::EmojiScope::Space);
        assert!(caps.stickers);

        // Catalog returns custom emoji and stickers filtered by query, and downloads images up front.
        let (tx, rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &Live::new().unwrap(), Command::Catalog { kind: CatalogKind::Emoji, space_id: None, query: "par".into(), cursor: None, reply: tx });
        let page = rx.recv().unwrap().unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].name, ":party:");

        let (tx, rx) = std::sync::mpsc::channel();
        serve(&ctx, &saved, &Live::new().unwrap(), Command::Catalog { kind: CatalogKind::Stickers, space_id: None, query: "".into(), cursor: None, reply: tx });
        let page = rx.recv().unwrap().unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "s-1");

        // Send message with custom emoji span + sticker_id.
        let spans = vec![protocol::Span {
            start: 4,
            end: 11,
            style: protocol::Style::Emoji { file: MediaFile::new("tiny-emoji-e-1", MediaKind::Image) },
        }];
        let msg = post_message(&ctx, &saved, "chan-1", "c-20", "yay :party:", None, &[], &spans, Some("s-1")).unwrap();
        assert!(msg.spans.iter().any(|s| matches!(&s.style, protocol::Style::Emoji { file } if file.id == "tiny-emoji-e-1")));
        assert!(msg.attachments.iter().any(|a| a.sticker && a.file.id == "tiny-emoji-s-1"));
        assert_eq!(msg.reactions.len(), 1);
        assert!(msg.reactions[0].file.is_some());

        // Custom reaction sends {emoji_id}.
        let (tx, rx) = std::sync::mpsc::channel();
        react(&ctx, &saved, "chan-1", "m-20", "tiny-emoji-e-1", true, tx);
        assert!(rx.recv().unwrap().is_ok());
        let react_req = log.lock().unwrap().iter().find(|r| r.starts_with("PUT /v1/messages/m-20/reactions ")).cloned().unwrap();
        let react_body: Value = serde_json::from_str(react_req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(react_body["emoji_id"], "e-1");

        // Deleted/unknown emoji id stays as plain `:gone:` text (no Style::Emoji span).
        let wire: WireMessage = serde_json::from_value(json!({
            "id": "m-21",
            "author_id": PEER,
            "body": ":gone:",
            "created_at": WHEN,
            "emoji": [{"id": "e-deleted", "start": 0, "end": 6}]
        })).unwrap();
        let mut media = Vec::new();
        let fallback = map_message(&saved, &HashMap::new(), &wire, &HashMap::new(), &mut media).unwrap();
        assert!(!fallback.spans.iter().any(|s| matches!(s.style, protocol::Style::Emoji { .. })));
    }

    #[test]
    fn link_previews_capability_and_receive_forces_real_host() {
        let (_adapter, _ctx, saved) = fixture(|request| {
            if request.starts_with("GET /v1/capabilities") {
                return Some(http(200, "OK", r#"{"version":"0.6.0","features":["link_previews"]}"#));
            }
            if request.starts_with("GET /v1/server ") {
                return Some(http(200, "OK", r#"{"name":"Tiny"}"#));
            }
            None
        });
        let caps = account_caps(&saved);
        assert!(caps.suppress_embeds, "link_previews feature should enable composer link chip");

        // Plaintext receive: spoofed site name is overridden with real host of url; image comes from Tiny server.
        let wire: WireMessage = serde_json::from_value(json!({
            "id": "m-prev-1",
            "author_id": PEER,
            "body": "Read https://actual.example.org/story",
            "created_at": WHEN,
            "previews": [{
                "url": "https://actual.example.org/story",
                "site": "bank-of-trust.com",
                "title": "Real Story",
                "description": "Details here",
                "image_attachment_id": "img-prev-1"
            }]
        })).unwrap();
        let mut media = Vec::new();
        let msg = map_message(&saved, &HashMap::new(), &wire, &HashMap::new(), &mut media).unwrap();
        assert_eq!(msg.previews.len(), 1);
        assert_eq!(msg.previews[0].site, "actual.example.org");
        assert_eq!(msg.previews[0].title, "Real Story");
        assert_eq!(msg.previews[0].image.as_ref().map(|f| f.id.as_str()), Some("img-prev-1"));
        assert!(media.iter().any(|(f, url)| f.id == "img-prev-1" && url.ends_with("/v1/media/img-prev-1")));

        // Sealed receive: same host enforcement + Tiny server media registration.
        let sealed_wire: WireMessage = serde_json::from_value(json!({
            "id": "m-prev-2",
            "author_id": PEER,
            "body": "",
            "created_at": WHEN,
            "e2ee": true,
            "kind": "message",
            "ciphertext": "AQID"
        })).unwrap();
        let opened = e2ee::Opened {
            text: "See https://docs.example.net/guide".into(),
            previews: vec![e2ee::OpenedPreview {
                url: "https://docs.example.net/guide".into(),
                site: "spoofed.evil".into(),
                title: "Guide".into(),
                description: "Docs".into(),
                image: Some(e2ee::OpenedFile {
                    id: "img-prev-sealed".into(),
                    name: "preview.jpg".into(),
                    mime: "image/jpeg".into(),
                    bytes: 1234,
                    width: 640,
                    height: 360,
                    ..e2ee::OpenedFile::default()
                }),
            }],
            ..e2ee::Opened::default()
        };
        let mut opened_map = HashMap::new();
        opened_map.insert("m-prev-2".into(), Read::Opened(opened));
        let mut sealed_media = Vec::new();
        let smsg = map_message(&saved, &HashMap::new(), &sealed_wire, &opened_map, &mut sealed_media).unwrap();
        assert_eq!(smsg.previews.len(), 1);
        assert_eq!(smsg.previews[0].site, "docs.example.net");
        assert_eq!(smsg.previews[0].image.as_ref().map(|f| f.id.as_str()), Some("img-prev-sealed"));
        assert!(sealed_media.iter().any(|(f, url)| f.id == "img-prev-sealed" && url.ends_with("/v1/media/img-prev-sealed")));
    }

    #[test]
    fn threads_capability_history_send_read_live_and_open_threads() {
        use std::sync::{Arc, Mutex};
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let log_srv = log.clone();
        let (_adapter, ctx, saved) = fixture(move |request| {
            log_srv.lock().unwrap().push(request.to_owned());
            if request.starts_with("GET /v1/capabilities") {
                return Some(http(200, "OK", r#"{"version":"0.6.0","features":["threads"]}"#));
            }
            if request.starts_with("GET /v1/server ") {
                return Some(http(200, "OK", r#"{"name":"Tiny"}"#));
            }
            if request.starts_with("GET /v1/conversations/chan-1/messages?limit=50&threads=1 ") {
                return Some(http(200, "OK", &json!({
                    "messages": [{
                        "id": "root-1",
                        "author_id": PEER,
                        "body": "Main topic",
                        "created_at": WHEN,
                        "thread": {
                            "replies": 1,
                            "last_reply_at": WHEN,
                            "participants": [ME],
                            "unread": 0
                        }
                    }]
                }).to_string()));
            }
            if request.starts_with("GET /v1/messages/root-1/thread?limit=50 ") {
                return Some(http(200, "OK", &json!({
                    "messages": [{
                        "id": "rep-1",
                        "author_id": PEER,
                        "body": "First reply",
                        "thread_id": "root-1",
                        "created_at": WHEN
                    }]
                }).to_string()));
            }
            if request.starts_with("POST /v1/conversations/chan-1/messages ") {
                return Some(http(201, "Created", &json!({
                    "id": "rep-2",
                    "author_id": ME,
                    "body": "Second reply",
                    "thread_id": "root-1",
                    "created_at": WHEN
                }).to_string()));
            }
            if request.starts_with("POST /v1/messages/root-1/thread/read ") {
                return Some(http(204, "No Content", ""));
            }
            if request.starts_with("PUT /v1/messages/root-1/thread/follow ")
                || request.starts_with("DELETE /v1/messages/root-1/thread/follow ")
            {
                return Some(http(204, "No Content", ""));
            }
            if request.starts_with("GET /v1/threads ") {
                return Some(http(200, "OK", &json!([{
                    "channel_id": "chan-1",
                    "message_id": "root-1",
                    "message": {
                        "id": "root-1",
                        "author_id": PEER,
                        "body": "Main topic",
                        "created_at": WHEN
                    },
                    "thread": {
                        "replies": 4,
                        "last_reply_at": WHEN,
                        "participants": [PEER, ME],
                        "unread": 4
                    }
                }]).to_string()));
            }
            None
        });

        let caps = account_caps(&saved);
        assert!(caps.threads, "threads feature must set Capabilities::threads");

        // Channel history uses ?threads=1 and attaches ThreadSummary to root.
        let mut ch = ConversationRow::new("chan-1", ConversationKind::Channel, "general");
        ch.space = Some(SPACE.to_owned());
        ctx.conversation(&ch).unwrap();
        fetch_page(&ctx, &saved, "chan-1", None, 50).unwrap();
        let root = ctx.stored_message("chan-1", "root-1").unwrap().unwrap();
        let summary = root.thread.as_ref().expect("root has thread summary");
        assert_eq!(summary.id, "chan-1:root-1");
        assert_eq!(summary.replies, 1);

        // Thread history reads GET /v1/messages/root-1/thread.
        fetch_page(&ctx, &saved, "chan-1:root-1", None, 50).unwrap();
        assert!(ctx.stored_message("chan-1:root-1", "rep-1").unwrap().is_some());

        // Sending in thread conversation posts to parent channel with thread_id.
        let sent = post_message(&ctx, &saved, "chan-1:root-1", "c-t1", "Second reply", None, &[], &[], None).unwrap();
        assert_eq!(sent.id, "rep-2");
        let post_req = log.lock().unwrap().iter().find(|r| r.starts_with("POST /v1/conversations/chan-1/messages ")).cloned().unwrap();
        let post_body: Value = serde_json::from_str(post_req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(post_body["thread_id"], "root-1");

        // MarkRead on thread posts to /v1/messages/{root}/thread/read.
        mark_read(&saved, "chan-1:root-1", "rep-2");
        assert!(log.lock().unwrap().iter().any(|r| r.starts_with("POST /v1/messages/root-1/thread/read ")));

        // Follow & unfollow helpers hit PUT/DELETE /v1/messages/{root}/thread/follow.
        follow_thread(&saved, "root-1", true).unwrap();
        follow_thread(&saved, "root-1", false).unwrap();

        // Live thread.updated refreshes root's ThreadSummary.
        on_live(&ctx, &saved, &HashMap::new(), "thread.updated", &json!({
            "conversation_id": "chan-1",
            "kind": "channel",
            "message_id": "root-1",
            "thread": { "replies": 3, "last_reply_at": WHEN, "participants": [PEER, ME], "unread": 2 }
        }));
        let updated = ctx.stored_message("chan-1", "root-1").unwrap().unwrap();
        assert_eq!(updated.thread.as_ref().unwrap().replies, 3);
        assert_eq!(updated.thread.as_ref().unwrap().unread, 2);

        // Live reply with thread_id routes to thread conversation, not channel.
        let reply_wire: WireMessage = serde_json::from_value(json!({
            "id": "rep-3",
            "author_id": PEER,
            "body": "Live thread reply",
            "thread_id": "root-1",
            "created_at": WHEN
        })).unwrap();
        deliver(&ctx, &saved, &HashMap::new(), "chan-1", &reply_wire);
        assert!(ctx.stored_message("chan-1:root-1", "rep-3").unwrap().is_some());
        assert!(ctx.stored_message("chan-1", "rep-3").unwrap().is_none());

        // OpenThreads loads GET /v1/threads and marks followed.
        open_threads(&ctx, &saved);
        let thread_row = ctx.known("chan-1:root-1").unwrap().unwrap();
        assert!(thread_row.followed);
        assert_eq!(thread_row.unread, 4);
    }
}
