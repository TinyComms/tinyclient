//! The picker's catalogs: saved GIFs, a GIF search through Telegram's `@gif` inline bot, and
//! the installed sticker packs one pack a page. Every preview is a still (a GIF's or sticker's
//! thumbnail); nothing animated is fetched. Asked only when a picker tab opens or the query
//! changes, never ahead. What a pick names is remembered here, so sending it uploads nothing.

use super::content;
use super::peers::Who;
use super::wire::Fault;
use super::Account;
use crate::adapters::CatalogPage;
use grammers_tl_types as tl;
use protocol::{CatalogItem, CatalogKind};

/// Picks remembered, newest last: enough for a few pages of every tab.
const PICKS: usize = 1_000;

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
    account.files.want(wants);
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

pub(super) fn catalog(account: &Account, kind: CatalogKind, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Fault> {
    match kind {
        // Custom emoji are Premium's; this account is offered none (`Capabilities`).
        CatalogKind::Emoji => Ok(CatalogPage::default()),
        CatalogKind::Gifs if query.is_empty() => saved_gifs(account),
        CatalogKind::Gifs => gif_search(account, query, cursor.unwrap_or("")),
        CatalogKind::Stickers => stickers(account, cursor.and_then(|cursor| cursor.parse().ok()).unwrap_or(0)),
    }
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

fn gif_search(account: &Account, query: &str, offset: &str) -> Result<CatalogPage, Fault> {
    let (bot, _) = gif_bot(account)?;
    let request = tl::functions::messages::GetInlineBotResults {
        bot,
        // The query is asked in no chat: the bot sees only the words.
        peer: tl::enums::InputPeer::PeerSelf,
        geo_point: None,
        query: query.to_owned(),
        offset: offset.to_owned(),
    };
    let tl::enums::messages::BotResults::Results(found) = account.ask(&request)?;
    account.learn(&found.users, &[]);
    let mut items = Vec::new();
    for result in &found.results {
        let (id, document) = match result {
            tl::enums::BotInlineResult::BotInlineMediaResult(result) => (result.id.clone(), result.document.as_ref()),
            tl::enums::BotInlineResult::Result(result) => (result.id.clone(), None),
        };
        let pick_id = format!("i{}:{id}", found.query_id);
        let Some(mut shown) = document.and_then(|document| item(account, document, "")) else {
            continue;
        };
        // Sent as the bot's result, not as the document: that is what the bot was asked for.
        remember(account, &pick_id, Pick::Inline { query_id: found.query_id, result: id });
        shown.id = pick_id;
        items.push(shown);
    }
    Ok(CatalogPage { items, cursor: found.next_offset.filter(|next| !next.is_empty()) })
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
