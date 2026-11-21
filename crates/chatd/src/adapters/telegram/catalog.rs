//! The picker's catalogs: saved GIFs, a GIF search through Telegram's `@gif` inline bot, the
//! installed sticker packs one pack a page, and the installed custom-emoji packs the same way.
//! Every preview is a still (a GIF's or sticker's thumbnail); nothing animated is fetched.
//! Asked only when a picker tab opens or the query changes, never ahead. What a pick names is
//! remembered here, so sending it uploads nothing. A custom emoji is also remembered by the
//! `:alt:` the composer inserts, which is all a send is given.

use super::content;
use super::peers::Who;
use super::wire::Fault;
use super::Account;
use crate::adapters::CatalogPage;
use grammers_tl_types as tl;
use protocol::{CatalogItem, CatalogKind};

/// Picks remembered, newest last: enough for a few pages of every tab.
const PICKS: usize = 1_000;
/// `:alt:` labels remembered for custom emoji, newest last.
const EMOJI_LABELS: usize = 2_000;

/// What a catalog item sends.
#[derive(Debug, Clone)]
pub(super) enum Pick {
    /// A saved GIF or a sticker: the document itself, sent as it is.
    Document(tl::types::InputDocument),
    /// A result of an inline query (`@gif`): sent through the bot, as Telegram's apps do.
    Inline { query_id: i64, result: String },
}

pub(super) fn remember(account: &Account, id: &str, pick: Pick) {
    let mut live = account.live();
    if live.picks.len() >= PICKS {
        live.picks.pop_front();
    }
    live.picks.push_back((id.to_owned(), pick));
}

pub(super) fn find(account: &Account, id: &str) -> Option<Pick> {
    account.live().picks.iter().rev().find(|(held, _)| held == id).map(|(_, pick)| pick.clone())
}

/// Remembers a document from a message so `save_gif` can address it by `d<id>` without re-fetching.
pub(super) fn remember_document(account: &Account, loc: &tl::types::InputDocumentFileLocation) {
    let id = format!("d{}", loc.id);
    if find(account, &id).is_some() {
        return;
    }
    remember(
        account,
        &id,
        Pick::Document(tl::types::InputDocument {
            id: loc.id,
            access_hash: loc.access_hash,
            file_reference: loc.file_reference.clone(),
        }),
    );
}

/// Saves or removes a GIF in Telegram's saved GIFs list (`messages.saveGif`).
pub(super) fn save_gif(account: &Account, id: &str, unsave: bool) -> Result<(), Fault> {
    let Some(Pick::Document(doc)) = find(account, id) else {
        return Err(Fault::Rpc { code: 400, name: "DOCUMENT_INVALID".to_owned(), value: None });
    };
    account.ask(&tl::functions::messages::SaveGif { id: doc.into(), unsave })?;
    Ok(())
}

/// A document as a picker item: its still, queued to be fetched, and remembered for sending.
fn item(account: &Account, document: &tl::enums::Document, pack: &str) -> Option<CatalogItem> {
    let tl::enums::Document::Document(document) = document else {
        return None;
    };
    let media = tl::types::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: false,
        round: false,
        voice: false,
        document: None,
        alt_documents: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    };
    let mut wants = Vec::new();
    let attachment = content::document(&media, document, &mut wants);
    let name = document.attributes.iter().find_map(|attribute| match attribute {
        tl::enums::DocumentAttribute::Sticker(sticker) => Some(sticker.alt.clone()),
        _ => None,
    });
    let animated = matches!(document.mime_type.as_str(), "application/x-tgsticker" | "video/webm" | "video/mp4" | "image/gif");
    // A pack shows stills. The animation is fetched when a message shows the sticker, not for
    // every cell of a pack at once.
    if attachment.sticker {
        if let Some(still) = &attachment.thumb {
            if still.id != attachment.file.id {
                wants.retain(|want| want.file.id == still.id);
            }
        }
    }
    // A picker's tile, not a message's picture: it keeps its turn when the picker asks for it.
    account.files.want_tiles(wants);
    let id = format!("d{}", document.id);
    remember(
        account,
        &id,
        Pick::Document(tl::types::InputDocument { id: document.id, access_hash: document.access_hash, file_reference: document.file_reference.clone() }),
    );
    Some(CatalogItem {
        id,
        name: name.unwrap_or_default(),
        // A still only: the thumbnail, never the animation.
        file: attachment.thumb,
        pack: pack.to_owned(),
        animated,
    })
}

pub(super) fn catalog(
    account: &Account,
    kind: CatalogKind,
    space: Option<&str>,
    query: &str,
    cursor: Option<&str>,
) -> Result<CatalogPage, Fault> {
    match kind {
        CatalogKind::Emoji => emojis(account, cursor.and_then(|cursor| cursor.parse().ok()).unwrap_or(0)),
        CatalogKind::Gifs if query.is_empty() => saved_gifs(account),
        CatalogKind::Gifs => inline_search(account, query, cursor.unwrap_or(""), space),
        CatalogKind::Stickers => stickers(account, cursor.and_then(|cursor| cursor.parse().ok()).unwrap_or(0)),
    }
}

/// `:alt:` for one document, so a later send can put the glyph back and name the document.
fn remember_label(account: &Account, label: String, document_id: i64, fallback: String) {
    if label.is_empty() || fallback.is_empty() {
        return;
    }
    let mut live = account.live();
    if let Some(held) = live.emoji_labels.iter_mut().find(|held| held.label == label) {
        if held.document_id != document_id {
            held.ambiguous = true;
        }
        return;
    }
    if live.emoji_labels.len() >= EMOJI_LABELS {
        live.emoji_labels.pop_front();
    }
    live.emoji_labels.push_back(content::EmojiLabel { label, document_id, fallback, ambiguous: false });
}

/// A custom-emoji document as a picker item. The id is `e<document>`, which is what a span
/// names, and the name is the alt glyph the composer wraps in colons.
fn emoji_item(account: &Account, document: &tl::enums::Document, pack: &str) -> Option<CatalogItem> {
    let tl::enums::Document::Document(raw) = document else {
        return None;
    };
    let mut shown = item(account, document, pack)?;
    let alt = raw.attributes.iter().find_map(|attribute| match attribute {
        tl::enums::DocumentAttribute::CustomEmoji(emoji) if !emoji.alt.is_empty() => Some(emoji.alt.clone()),
        _ => None,
    });
    shown.id = format!("e{}", raw.id);
    match alt {
        Some(alt) => {
            remember_label(account, format!(":{alt}:"), raw.id, alt.clone());
            shown.name = alt;
        }
        None => {
            shown.name = shown.id.clone();
            remember_label(account, format!(":{}:", shown.id), raw.id, "\u{2606}".to_owned());
        }
    }
    Some(shown)
}

/// Emoji used lately, ahead of the first pack. A miss here still leaves the packs.
fn recent_emoji(account: &Account) -> Vec<CatalogItem> {
    let found = account.ask(&tl::functions::messages::GetRecentReactions { limit: 50, hash: 0 });
    let ids: Vec<i64> = match found {
        Ok(tl::enums::messages::Reactions::Reactions(found)) => found
            .reactions
            .iter()
            .filter_map(|reaction| match reaction {
                tl::enums::Reaction::CustomEmoji(emoji) => Some(emoji.document_id),
                _ => None,
            })
            .collect(),
        _ => return Vec::new(),
    };
    if ids.is_empty() {
        return Vec::new();
    }
    let documents = match account.ask(&tl::functions::messages::GetCustomEmojiDocuments { document_id: ids }) {
        Ok(documents) => documents,
        Err(_) => return Vec::new(),
    };
    documents.iter().filter_map(|document| emoji_item(account, document, "Recent")).take(protocol::CATALOG_LIMIT).collect()
}

/// One installed emoji pack a page, in the user's order. The first page leads with recent emoji.
fn emojis(account: &Account, at: usize) -> Result<CatalogPage, Fault> {
    let held = account.live().emoji_sets.clone().filter(|_| at > 0);
    let sets = match held {
        Some(sets) => sets,
        None => match account.ask(&tl::functions::messages::GetEmojiStickers { hash: 0 })? {
            tl::enums::messages::AllStickers::Stickers(all) => {
                let sets: Vec<(i64, i64, String)> =
                    all.sets.iter().map(|tl::enums::StickerSet::Set(set)| (set.id, set.access_hash, set.title.clone())).collect();
                account.live().emoji_sets = Some(sets.clone());
                sets
            }
            tl::enums::messages::AllStickers::NotModified => {
                account.live().emoji_sets = Some(Vec::new());
                Vec::new()
            }
        },
    };
    let mut items = Vec::new();
    if at == 0 {
        items.extend(recent_emoji(account));
    }
    if let Some((id, access_hash, title)) = sets.get(at).cloned() {
        let request = tl::functions::messages::GetStickerSet { stickerset: tl::types::InputStickerSetId { id, access_hash }.into(), hash: 0 };
        match account.ask(&request) {
            Ok(tl::enums::messages::StickerSet::Set(set)) => {
                let room = protocol::CATALOG_LIMIT.saturating_sub(items.len());
                items.extend(set.documents.iter().take(room).filter_map(|document| emoji_item(account, document, &title)));
            }
            Ok(tl::enums::messages::StickerSet::NotModified) => {}
            Err(fault) if items.is_empty() => return Err(fault),
            Err(_) => {}
        }
    }
    let cursor = (at + 1 < sets.len()).then(|| (at + 1).to_string());
    Ok(CatalogPage { items, cursor })
}

fn saved_gifs(account: &Account) -> Result<CatalogPage, Fault> {
    let tl::enums::messages::SavedGifs::Gifs(found) = account.ask(&tl::functions::messages::GetSavedGifs { hash: 0 })? else {
        return Ok(CatalogPage::default());
    };
    let items = found.gifs.iter().take(protocol::CATALOG_LIMIT).filter_map(|gif| item(account, gif, "Saved")).collect();
    Ok(CatalogPage { items, cursor: None })
}

/// The bot Telegram's apps search GIFs with, resolved once and remembered.
// Telegram names it in `help.getConfig` (`gif_search_username`); it has been `gif` for years
// and asking the whole config for one name is not worth a request (unverified for every region).
fn gif_bot(account: &Account) -> Result<(tl::enums::InputUser, tl::enums::InputPeer), Fault> {
    let bot = Who::User(match account.ctx.note("gifbot").and_then(|id| id.parse().ok()) {
        Some(id) => id,
        None => {
            let request = tl::functions::contacts::ResolveUsername { username: "gif".to_owned(), referer: None };
            let tl::enums::contacts::ResolvedPeer::Peer(found) = account.ask(&request)?;
            account.learn(&found.users, &found.chats);
            let Who::User(id) = Who::of(&found.peer) else {
                return Err(Fault::Rpc { code: 400, name: "BOT_INVALID".to_owned(), value: None });
            };
            account.ctx.set_note("gifbot", &id.to_string());
            id
        }
    });
    let peer = account.input(bot)?;
    let user = account.dir().input_user(bot).ok_or_else(|| Fault::Rpc { code: 400, name: "BOT_INVALID".to_owned(), value: None })?;
    Ok((user, peer))
}

/// `@username rest` is that inline bot and the words after it. Anything else is a GIF search.
pub(super) fn bot_query(query: &str) -> Option<(&str, &str)> {
    let rest = query.trim().strip_prefix('@')?;
    let (name, words) = match rest.split_once(|c: char| c.is_whitespace()) {
        Some((name, words)) => (name, words.trim()),
        None => (rest, ""),
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((name, words))
}

/// One bot, resolved once and remembered. `@gif` keeps the note GIF search already used.
fn inline_bot(account: &Account, username: &str) -> Result<tl::enums::InputUser, Fault> {
    if username.eq_ignore_ascii_case("gif") {
        return gif_bot(account).map(|(user, _)| user);
    }
    let note = format!("inline:{username}");
    let bot = Who::User(match account.ctx.note(&note).and_then(|id| id.parse().ok()) {
        Some(id) => id,
        None => {
            let request = tl::functions::contacts::ResolveUsername { username: username.to_owned(), referer: None };
            let tl::enums::contacts::ResolvedPeer::Peer(found) = account.ask(&request)?;
            account.learn(&found.users, &found.chats);
            let Who::User(id) = Who::of(&found.peer) else {
                return Err(Fault::Rpc { code: 400, name: "BOT_INVALID".to_owned(), value: None });
            };
            account.ctx.set_note(&note, &id.to_string());
            id
        }
    });
    account.dir().input_user(bot).ok_or_else(|| Fault::Rpc { code: 400, name: "BOT_INVALID".to_owned(), value: None })
}

/// The open chat when the picker was asked from one, so the bot sees that chat. Otherwise the
/// bot sees only the words. A bot that wants a location is not given one: there is no confirm.
fn inline_peer(account: &Account, space: Option<&str>) -> tl::enums::InputPeer {
    space.and_then(Who::parse).and_then(|(who, _)| account.input(who).ok()).unwrap_or(tl::enums::InputPeer::PeerSelf)
}

fn inline_search(account: &Account, query: &str, offset: &str, space: Option<&str>) -> Result<CatalogPage, Fault> {
    let (username, words) = bot_query(query).unwrap_or(("gif", query));
    let bot = inline_bot(account, username)?;
    let request = tl::functions::messages::GetInlineBotResults {
        bot,
        peer: inline_peer(account, space),
        // Never a location. A bot that asks for one would need a confirm this build does not have.
        geo_point: None,
        query: words.to_owned(),
        offset: offset.to_owned(),
    };
    let tl::enums::messages::BotResults::Results(found) = account.ask(&request)?;
    account.learn(&found.users, &[]);
    // `switch_pm` and `switch_webview` have nowhere to go on a catalog page, so they are not shown.
    let mut items = Vec::new();
    for result in &found.results {
        let pick_id = format!("i{}:{}", found.query_id, inline_id(result));
        match result {
            tl::enums::BotInlineResult::BotInlineMediaResult(result) => {
                let Some(mut shown) = result.document.as_ref().and_then(|document| item(account, document, "")) else {
                    continue;
                };
                remember(account, &pick_id, Pick::Inline { query_id: found.query_id, result: result.id.clone() });
                shown.id = pick_id;
                items.push(shown);
            }
            tl::enums::BotInlineResult::Result(result) => {
                remember(account, &pick_id, Pick::Inline { query_id: found.query_id, result: result.id.clone() });
                items.push(CatalogItem {
                    id: pick_id,
                    name: result.title.clone().unwrap_or_default(),
                    file: None,
                    pack: result.description.clone().unwrap_or_default(),
                    animated: false,
                });
            }
        }
    }
    Ok(CatalogPage { items, cursor: found.next_offset.filter(|next| !next.is_empty()) })
}

fn inline_id(result: &tl::enums::BotInlineResult) -> &str {
    match result {
        tl::enums::BotInlineResult::BotInlineMediaResult(result) => &result.id,
        tl::enums::BotInlineResult::Result(result) => &result.id,
    }
}

/// One installed pack a page, in the user's order: the list of packs, then that pack.
fn stickers(account: &Account, at: usize) -> Result<CatalogPage, Fault> {
    let held = account.live().sticker_sets.clone().filter(|_| at > 0);
    let sets = match held {
        Some(sets) => sets,
        None => {
            let tl::enums::messages::AllStickers::Stickers(all) = account.ask(&tl::functions::messages::GetAllStickers { hash: 0 })? else {
                return Ok(CatalogPage::default());
            };
            let sets: Vec<(i64, i64, String)> = all
                .sets
                .iter()
                .map(|tl::enums::StickerSet::Set(set)| (set.id, set.access_hash, set.title.clone()))
                .collect();
            account.live().sticker_sets = Some(sets.clone());
            sets
        }
    };
    let Some((id, access_hash, title)) = sets.get(at).cloned() else {
        return Ok(CatalogPage::default());
    };
    let request = tl::functions::messages::GetStickerSet { stickerset: tl::types::InputStickerSetId { id, access_hash }.into(), hash: 0 };
    let tl::enums::messages::StickerSet::Set(set) = account.ask(&request)? else {
        return Ok(CatalogPage::default());
    };
    let items = set.documents.iter().take(protocol::CATALOG_LIMIT).filter_map(|sticker| item(account, sticker, &title)).collect();
    Ok(CatalogPage { items, cursor: (at + 1 < sets.len()).then(|| (at + 1).to_string()) })
}
