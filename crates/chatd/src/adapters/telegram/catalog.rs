//! The picker's catalogs: saved GIFs, a GIF search through Telegram's `@gif` inline bot, sticker
//! packs one page at a time (recent, favourites, featured, then each installed pack), a sticker
//! search, and the installed custom-emoji packs the same way.
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
    /// `sticker` asks Telegram to move that pack up in the user's order.
    Document { document: tl::types::InputDocument, sticker: bool },
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
        Pick::Document {
            document: tl::types::InputDocument {
                id: loc.id,
                access_hash: loc.access_hash,
                file_reference: loc.file_reference.clone(),
            },
            sticker: false,
        },
    );
}

/// Saves or removes a GIF in Telegram's saved GIFs list (`messages.saveGif`).
pub(super) fn save_gif(account: &Account, id: &str, unsave: bool) -> Result<(), Fault> {
    let Some(Pick::Document { document: doc, .. }) = find(account, id) else {
        return Err(Fault::Rpc { code: 400, name: "DOCUMENT_INVALID".to_owned(), value: None });
    };
    account.ask(&tl::functions::messages::SaveGif { id: doc.into(), unsave })?;
    Ok(())
}

/// One page of the Stickers tab, kept so the next page does not ask for the list again.
#[derive(Debug, Clone)]
pub(super) enum StickerPage {
    Recent,
    Faved,
    Pack { id: i64, access: i64, title: String, locked: bool, set: String },
}

/// A document as a picker item: its still, queued to be fetched, and remembered for sending.
/// `set` is the install key when the pack is not on the account yet. It stays empty when the
/// pack is installed, and when it is locked, so the picker does not offer Add. `locked` is a
/// Premium sticker this account cannot send.
fn item(account: &Account, document: &tl::enums::Document, pack: &str, locked: bool, set: &str) -> Option<CatalogItem> {
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
        Pick::Document {
            document: tl::types::InputDocument { id: document.id, access_hash: document.access_hash, file_reference: document.file_reference.clone() },
            sticker: attachment.sticker,
        },
    );
    Some(CatalogItem {
        id,
        name: name.unwrap_or_default(),
        // A still only: the thumbnail, never the animation.
        file: attachment.thumb,
        pack: pack.to_owned(),
        animated,
        set: set.to_owned(),
        locked,
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
        CatalogKind::Stickers => stickers(account, query, cursor.and_then(|cursor| cursor.parse().ok()).unwrap_or(0)),
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
    let mut shown = item(account, document, pack, false, "")?;
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
    let items = found.gifs.iter().take(protocol::CATALOG_LIMIT).filter_map(|gif| item(account, gif, "Saved", false, "")).collect();
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
                let Some(mut shown) = result.document.as_ref().and_then(|document| item(account, document, "", false, "")) else {
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
                    set: String::new(),
                    locked: false,
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

/// Recent and favourite stickers, then featured packs, then one installed pack a page.
/// A word or an emoji in `query` searches instead of paging.
fn stickers(account: &Account, query: &str, at: usize) -> Result<CatalogPage, Fault> {
    if !query.is_empty() {
        return searched(account, query);
    }
    let held = account.live().sticker_sets.clone().filter(|_| at > 0);
    let pages = match held {
        Some(pages) => pages,
        None => {
            let pages = sticker_pages(account)?;
            account.live().sticker_sets = Some(pages.clone());
            pages
        }
    };
    let Some(page) = pages.get(at).cloned() else {
        return Ok(CatalogPage::default());
    };
    let cursor = (at + 1 < pages.len()).then(|| (at + 1).to_string());
    let (title, locked, set, documents) = match page {
        StickerPage::Recent => ("Recent".to_owned(), false, String::new(), recent_documents(account)?),
        StickerPage::Faved => ("Favourites".to_owned(), false, String::new(), faved_documents(account)?),
        StickerPage::Pack { id, access, title, locked, set } => {
            let request = tl::functions::messages::GetStickerSet { stickerset: tl::types::InputStickerSetId { id, access_hash: access }.into(), hash: 0 };
            let documents = match account.ask(&request)? {
                tl::enums::messages::StickerSet::Set(found) => found.documents,
                tl::enums::messages::StickerSet::NotModified => Vec::new(),
            };
            (title, locked, set, documents)
        }
    };
    let items = documents
        .iter()
        .take(protocol::CATALOG_LIMIT)
        .filter_map(|document| {
            let install = installable(&pages, document, &set, locked);
            item(account, document, &title, locked, &install)
        })
        .collect();
    Ok(CatalogPage { items, cursor })
}

/// The set key to offer an Add for. A locked pack never has one. Otherwise the page's own key,
/// or the document's when that pack is not installed.
fn installable(pages: &[StickerPage], document: &tl::enums::Document, page_set: &str, locked: bool) -> String {
    if locked {
        return String::new();
    }
    if !page_set.is_empty() {
        return page_set.to_owned();
    }
    let tl::enums::Document::Document(raw) = document else {
        return String::new();
    };
    let key = content::sticker_set_key(&raw.attributes);
    let installed = pages.iter().any(|page| matches!(page, StickerPage::Pack { id, access, set, .. } if set.is_empty() && key == format!("{id}:{access}")));
    if key.is_empty() || installed { String::new() } else { key }
}

fn account_premium(account: &Account) -> bool {
    account.dir().get(Who::User(account.me)).is_some_and(|known| known.premium)
}

/// Ids of Premium featured packs this account cannot add. Empty when the account is Premium
/// or the featured list is not a Premium list. Fetched each time: a steer can change the list.
fn premium_pack_ids(account: &Account) -> Result<Vec<i64>, Fault> {
    if account_premium(account) {
        return Ok(Vec::new());
    }
    let tl::enums::messages::FeaturedStickers::Stickers(found) = account.ask(&tl::functions::messages::GetFeaturedStickers { hash: 0 })? else {
        return Ok(Vec::new());
    };
    if !found.premium {
        return Ok(Vec::new());
    }
    Ok(found.sets.into_iter().map(|covered| {
        let tl::enums::StickerSet::Set(set) = covered.set();
        set.id
    }).collect())
}

fn sticker_pages(account: &Account) -> Result<Vec<StickerPage>, Fault> {
    let installed = match account.ask(&tl::functions::messages::GetAllStickers { hash: 0 })? {
        tl::enums::messages::AllStickers::Stickers(all) => all.sets,
        _ => Vec::new(),
    };
    let mut pages = Vec::new();
    if !recent_documents(account)?.is_empty() {
        pages.push(StickerPage::Recent);
    }
    if !faved_documents(account)?.is_empty() {
        pages.push(StickerPage::Faved);
    }
    let mut premium_ids = Vec::new();
    if let tl::enums::messages::FeaturedStickers::Stickers(found) = account.ask(&tl::functions::messages::GetFeaturedStickers { hash: 0 })? {
        let locked = found.premium && !account_premium(account);
        for covered in found.sets {
            let tl::enums::StickerSet::Set(set) = covered.set();
            if locked {
                premium_ids.push(set.id);
            }
            let already = installed.iter().any(|one| matches!(one, tl::enums::StickerSet::Set(held) if held.id == set.id));
            if already {
                continue;
            }
            pages.push(StickerPage::Pack {
                id: set.id,
                access: set.access_hash,
                title: set.title,
                locked,
                set: if locked { String::new() } else { format!("{}:{}", set.id, set.access_hash) },
            });
        }
    }
    for set in installed {
        let tl::enums::StickerSet::Set(set) = set;
        let locked = premium_ids.contains(&set.id);
        pages.push(StickerPage::Pack { id: set.id, access: set.access_hash, title: set.title, locked, set: String::new() });
    }
    Ok(pages)
}

fn recent_documents(account: &Account) -> Result<Vec<tl::enums::Document>, Fault> {
    Ok(match account.ask(&tl::functions::messages::GetRecentStickers { attached: false, hash: 0 })? {
        tl::enums::messages::RecentStickers::Stickers(found) => found.stickers,
        tl::enums::messages::RecentStickers::NotModified => Vec::new(),
    })
}

fn faved_documents(account: &Account) -> Result<Vec<tl::enums::Document>, Fault> {
    Ok(match account.ask(&tl::functions::messages::GetFavedStickers { hash: 0 })? {
        tl::enums::messages::FavedStickers::Stickers(found) => found.stickers,
        tl::enums::messages::FavedStickers::NotModified => Vec::new(),
    })
}

/// A sticker search: words go to `messages.searchStickers`, an emoji to `messages.getStickers`.
fn searched(account: &Account, query: &str) -> Result<CatalogPage, Fault> {
    let letters = query.chars().any(|c| c.is_ascii_alphanumeric());
    let documents = if letters {
        match account.ask(&tl::functions::messages::SearchStickers {
            emojis: false,
            q: query.to_owned(),
            emoticon: String::new(),
            lang_code: vec!["en".to_owned()],
            offset: 0,
            limit: protocol::CATALOG_LIMIT as i32,
            hash: 0,
        })? {
            tl::enums::messages::FoundStickers::Stickers(found) => found.stickers,
            tl::enums::messages::FoundStickers::NotModified(_) => Vec::new(),
        }
    } else {
        match account.ask(&tl::functions::messages::GetStickers { emoticon: query.to_owned(), hash: 0 })? {
            tl::enums::messages::Stickers::Stickers(found) => found.stickers,
            tl::enums::messages::Stickers::NotModified => Vec::new(),
        }
    };
    let locked_ids = premium_pack_ids(account)?;
    let mut titles: Vec<(String, String)> = Vec::new();
    let mut items = Vec::new();
    for document in documents.iter().take(protocol::CATALOG_LIMIT) {
        let key = match document {
            tl::enums::Document::Document(raw) => content::sticker_set_key(&raw.attributes),
            tl::enums::Document::Empty(_) => String::new(),
        };
        let locked = key.split_once(':').and_then(|(id, _)| id.parse::<i64>().ok()).is_some_and(|id| locked_ids.contains(&id));
        let title = titled(account, &key, &mut titles);
        let install = if locked { "" } else { key.as_str() };
        if let Some(shown) = item(account, document, &title, locked, install) {
            items.push(shown);
        }
    }
    Ok(CatalogPage { items, cursor: None })
}

fn titled(account: &Account, key: &str, titles: &mut Vec<(String, String)>) -> String {
    if key.is_empty() {
        return String::new();
    }
    if let Some((_, title)) = titles.iter().find(|(held, _)| held == key) {
        return title.clone();
    }
    let title = input_set(key).ok().and_then(|stickerset| {
        match account.ask(&tl::functions::messages::GetStickerSet { stickerset, hash: 0 }) {
            Ok(tl::enums::messages::StickerSet::Set(found)) => match found.set {
                tl::enums::StickerSet::Set(set) => Some(set.title),
            },
            _ => None,
        }
    }).unwrap_or_default();
    titles.push((key.to_owned(), title.clone()));
    title
}

fn input_set(set: &str) -> Result<tl::enums::InputStickerSet, Fault> {
    if let Some((id, access)) = set.split_once(':') {
        if let (Ok(id), Ok(access_hash)) = (id.parse::<i64>(), access.parse::<i64>()) {
            return Ok(tl::types::InputStickerSetId { id, access_hash }.into());
        }
    }
    if set.is_empty() {
        return Err(Fault::Rpc { code: 400, name: "STICKERSET_INVALID".to_owned(), value: None });
    }
    Ok(tl::types::InputStickerSetShortName { short_name: set.to_owned() }.into())
}

/// Installs or removes a pack, or restacks the installed packs. Clears the page list when it changes.
/// A Premium featured pack is not installed for an account that is not Premium.
pub(super) fn sticker_set(account: &Account, set: &str, install: Option<bool>, order: &[i64]) -> Result<(), Fault> {
    if !order.is_empty() {
        account.ask(&tl::functions::messages::ReorderStickerSets { masks: false, emojis: false, order: order.to_vec() })?;
        account.live().sticker_sets = None;
        return Ok(());
    }
    let stickerset = input_set(set)?;
    match install {
        Some(true) => {
            if let tl::enums::InputStickerSet::Id(chosen) = &stickerset {
                if premium_pack_ids(account)?.contains(&chosen.id) {
                    return Err(Fault::Rpc { code: 403, name: "PREMIUM_ACCOUNT_REQUIRED".to_owned(), value: None });
                }
            }
            account.ask(&tl::functions::messages::InstallStickerSet { stickerset, archived: false })?;
        }
        Some(false) => {
            account.ask(&tl::functions::messages::UninstallStickerSet { stickerset })?;
        }
        None => return Err(Fault::Rpc { code: 400, name: "STICKERSET_INVALID".to_owned(), value: None }),
    }
    account.live().sticker_sets = None;
    Ok(())
}

/// Stars or unstars one sticker the picker has already shown.
pub(super) fn fave_sticker(account: &Account, id: &str, unfave: bool) -> Result<(), Fault> {
    let Some(Pick::Document { document, .. }) = find(account, id) else {
        return Err(Fault::Rpc { code: 400, name: "DOCUMENT_INVALID".to_owned(), value: None });
    };
    account.ask(&tl::functions::messages::FaveSticker { id: document.into(), unfave })?;
    Ok(())
}
