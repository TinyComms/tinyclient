//! The picker's catalogs: the servers' own emoji and stickers, kept from READY and their update
//! events, and Discord's GIF search. An empty GIF query is favourites, trending categories, then
//! one page of trending; a search is hits, then suggested terms. The shell debounces typing.

use super::super::{CatalogPage, Refusal};
use super::model::{self, GuildEmoji, GuildSticker};
use super::{refusal, Discord, Pickable};
use protocol::{CatalogItem, CatalogKind, EmojiScope, MediaFile, MediaKind, ReactionRules};
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Items a page of a server's own emoji or stickers holds.
const PAGE: usize = protocol::CATALOG_LIMIT;
/// Section names the shell already draws. American spelling, so they merge with its labels.
const FAVORITES: &str = "Favorites";
const FREQUENT: &str = "Frequently used";
const CATEGORIES: &str = "Categories";
const SUGGESTIONS: &str = "Suggestions";
const TRENDING: &str = "Trending";
/// How many frecency rows lead the catalog. Scores are read, never written.
const EMOJI_FREQUENT: usize = 36;
const STICKER_FREQUENT: usize = 16;
/// GIFs asked for at once: what a picker shows before it is scrolled.
const GIFS: u32 = 50;
/// A GIF search's answer is kept this long, so typing a query again, or opening the picker
/// again on trending, asks Discord nothing.
const GIFS_KEPT: Duration = Duration::from_secs(600);
/// GIF answers kept at most, across accounts; the oldest goes.
const GIFS_HELD: usize = 64;

/// GIF answers by account, query, and page offset: when, and what.
type GifAnswers = HashMap<(String, String, u32), (Instant, CatalogPage)>;
static GIF_ANSWERS: Mutex<Option<GifAnswers>> = Mutex::new(None);

/// Picture facts for a page URL, remembered while a page is built so a later favourite write
/// can fill `src`, size, and format. A message star that never opened the picker has none.
#[derive(Clone)]
struct GifFact {
    src: String,
    width: u32,
    height: u32,
    format: u32,
}
type GifFacts = HashMap<String, GifFact>;
static GIF_FACTS: Mutex<Option<GifFacts>> = Mutex::new(None);

#[derive(Deserialize)]
pub struct Pickables {
    guild_id: String,
    #[serde(default)]
    emojis: Option<Vec<GuildEmoji>>,
    #[serde(default)]
    stickers: Option<Vec<GuildSticker>>,
}

#[derive(Deserialize)]
struct Gif {
    /// The page Discord unfurls into the GIF: what is sent.
    #[serde(default)]
    url: String,
    #[serde(default)]
    title: String,
    /// The media in the format asked for: Tenor's `tinygif`, a small GIF the size of a cell.
    #[serde(default)]
    src: String,
    /// A still of it.
    #[serde(default)]
    preview: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
}

impl Gif {
    /// What a cell shows: the small moving GIF when there is one, so the grid plays without
    /// a single original fetched; else the still.
    fn shown(&self) -> &str {
        let path = self.src.split('?').next().unwrap_or("");
        if self.src.starts_with("https://") && path.ends_with(".gif") {
            &self.src
        } else {
            &self.preview
        }
    }
}

/// A query as it goes in an address.
fn escaped(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// A stable name for an address, for a preview's file id.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

impl Discord {
    /// A server's emoji or stickers changed.
    pub(super) fn pickables_changed(&self, changed: Pickables) {
        let mut state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get_mut(&changed.guild_id) else {
            return;
        };
        if let Some(emojis) = changed.emojis {
            info.emojis = emojis.into_iter().filter(|emoji| emoji.available).filter_map(|emoji| {
                let roles = emoji.roles.iter().map(|role| model::number(role)).collect();
                Some(Pickable { id: emoji.id?, name: emoji.name, animated: emoji.animated, format: 0, roles })
            }).collect();
        }
        if let Some(stickers) = changed.stickers {
            info.stickers = stickers.into_iter().filter(|sticker| sticker.available).map(|sticker| Pickable {
                animated: sticker.format_type != 1,
                format: sticker.format_type,
                id: sticker.id,
                name: sticker.name,
                roles: Box::default(),
            }).collect();
        }
    }

    /// One page of a picker's catalog. Emoji and stickers come from what READY said, with the
    /// account's favourites and frecency (read once from `settings-proto/2`) in front. Without
    /// Nitro only the server the message is said in (none in a DM) and no animated emoji, with
    /// it every server's, plus standard sticker packs. An emoji limited to roles we lack is
    /// never offered. GIFs are Discord's search: favourites and categories when nothing is
    /// typed, hits then suggestions when something is, and a cursor when a page is full.
    pub(super) fn catalog(&self, kind: CatalogKind, space: Option<&str>, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Refusal> {
        match kind {
            CatalogKind::Gifs => self.gifs(query, cursor),
            CatalogKind::Emoji | CatalogKind::Stickers => {
                self.ensure_frecency();
                if kind == CatalogKind::Stickers {
                    self.ensure_sticker_packs();
                }
                let from: usize = cursor.and_then(|cursor| cursor.parse().ok()).unwrap_or(0);
                let (items, files, more) = self.pickables(kind, space, query, from);
                self.ctx.media(&files).map_err(|err| Refusal::Refused(err.to_string()))?;
                Ok(CatalogPage { items, cursor: more.then(|| (from + PAGE).to_string()) })
            }
        }
    }

    /// Adds or removes one favourite emoji, sticker, or GIF. The write replaces only that list.
    /// Frecency scores are left byte for byte: a wrong score would replace the account's
    /// frecency on every device.
    pub(super) fn favourite(&self, kind: CatalogKind, id: &str, on: bool) -> Result<(), Refusal> {
        self.ensure_frecency();
        let bytes = crate::hub::lock(&self.state).frecency.clone();
        let lists = super::proto::decode_frecency(&bytes);
        let updated = match kind {
            CatalogKind::Emoji => {
                let key = self.emoji_favourite_key(id);
                let mut emojis = lists.favorite_emojis;
                if on {
                    if !emojis.iter().any(|held| same_emoji(held, &key)) {
                        emojis.push(key);
                    }
                } else {
                    emojis.retain(|held| !same_emoji(held, &key) && !same_emoji(held, id));
                }
                super::proto::with_favorite_emojis(&bytes, &emojis)
            }
            CatalogKind::Stickers => {
                let key = id.trim().to_owned();
                let mut stickers = lists.favorite_stickers;
                if on {
                    if !stickers.iter().any(|held| held == &key) {
                        stickers.push(key);
                    }
                } else {
                    stickers.retain(|held| held != &key);
                }
                super::proto::with_favorite_stickers(&bytes, &stickers)
            }
            CatalogKind::Gifs => {
                let url = id.trim();
                if url.is_empty() || url.starts_with("cat:") {
                    return Ok(());
                }
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(Refusal::Refused("A GIF favourite is its page address.".into()));
                }
                let url = url.to_owned();
                let mut gifs = lists.favorite_gifs;
                let changed = if on {
                    if gifs.iter().any(|gif| gif.url == url) {
                        false
                    } else {
                        let order = gifs.iter().map(|gif| gif.order).max().unwrap_or(0).saturating_add(1);
                        let fact = crate::hub::lock(&GIF_FACTS).as_ref().and_then(|facts| facts.get(&url).cloned());
                        let (src, width, height, format) = match fact {
                            Some(fact) => (fact.src, fact.width, fact.height, fact.format),
                            None => (url.clone(), 0, 0, 1),
                        };
                        gifs.push(super::proto::FavoriteGif { url, src, format, width, height, order });
                        true
                    }
                } else {
                    let before = gifs.len();
                    gifs.retain(|gif| gif.url != url);
                    gifs.len() != before
                };
                if !changed {
                    return Ok(());
                }
                super::proto::with_favorite_gifs(&bytes, &gifs)
            }
        };
        let body = json!({"settings": super::proto::encode_settings(&updated)});
        self.call("PATCH", "/users/@me/settings-proto/2", Some(body)).map_err(refusal)?;
        let mut state = crate::hub::lock(&self.state);
        // A gateway update that landed during the write is newer than the bytes we patched.
        if state.frecency == bytes {
            state.frecency = updated;
        }
        drop(state);
        if kind == CatalogKind::Gifs {
            self.forget_gif_pages();
        }
        Ok(())
    }

    /// Drops cached GIF pages for this account. A favourite write or a gateway frecency update
    /// changed the list the next empty query should lead with.
    pub(super) fn forget_gif_pages(&self) {
        let account = &self.ctx.account.account;
        if let Some(held) = crate::hub::lock(&GIF_ANSWERS).as_mut() {
            held.retain(|(held_account, _, _), _| held_account != account);
        }
    }

    /// `settings-proto/2` once per session, the first time the emoji or sticker picker opens.
    /// A failure still counts, so a down route is not asked again on every keystroke.
    fn ensure_frecency(&self) {
        if crate::hub::lock(&self.state).frecency_loaded {
            return;
        }
        let bytes = self
            .call("GET", "/users/@me/settings-proto/2", None)
            .ok()
            .as_deref()
            .and_then(frecency_bytes)
            .unwrap_or_default();
        let mut state = crate::hub::lock(&self.state);
        if state.frecency_loaded {
            return;
        }
        state.frecency = bytes;
        state.frecency_loaded = true;
    }

    /// `GET /sticker-packs` once per session, and only with Nitro. Without it the route is
    /// not asked. A failure still counts.
    fn ensure_sticker_packs(&self) {
        {
            let state = crate::hub::lock(&self.state);
            if state.standard_stickers_loaded || !matches!(state.premium, 1..=3) {
                return;
            }
        }
        let packs = self.call("GET", "/sticker-packs", None).ok().as_deref().map(parse_sticker_packs).unwrap_or_default();
        let mut state = crate::hub::lock(&self.state);
        if state.standard_stickers_loaded {
            return;
        }
        state.standard_stickers = packs;
        state.standard_stickers_loaded = true;
    }

    /// The emoji string stored in `favorite_emojis`: `name:id`, `a:name:id` when animated, or
    /// the Unicode glyph. A catalog id `emoji:{snowflake}` is looked up for its name.
    fn emoji_favourite_key(&self, id: &str) -> String {
        match emoji_token(id) {
            EmojiToken::Unicode(glyph) => glyph,
            EmojiToken::Custom { animated, name, id: snowflake } => {
                let state = crate::hub::lock(&self.state);
                for info in state.guilds.values() {
                    if let Some(emoji) = info.emojis.iter().find(|emoji| emoji.id == snowflake) {
                        let label = emoji.name.trim_matches(':');
                        return if emoji.animated {
                            format!("a:{label}:{snowflake}")
                        } else {
                            format!("{label}:{snowflake}")
                        };
                    }
                }
                if !name.is_empty() {
                    return if animated { format!("a:{name}:{snowflake}") } else { format!("{name}:{snowflake}") };
                }
                snowflake
            }
        }
    }

    fn pickables(&self, kind: CatalogKind, space: Option<&str>, query: &str, from: usize) -> (Vec<CatalogItem>, Vec<(MediaFile, String)>, bool) {
        let state = crate::hub::lock(&self.state);
        let everywhere = matches!(state.premium, 1..=3);
        let lists = super::proto::decode_frecency(&state.frecency);
        let query = query.trim().trim_matches(':').to_lowercase();
        let mut guilds: Vec<(&String, &super::GuildInfo)> = state.guilds.iter().collect();
        // The server it is said in first, then the user's own order.
        guilds.sort_by_key(|(id, info)| (Some(id.as_str()) != space, info.position));
        let mut offered: Vec<Offered> = Vec::new();
        let mut seen = HashSet::new();
        let remember = |offered: &mut Vec<Offered>, seen: &mut HashSet<String>, item: CatalogItem, file: Option<(MediaFile, String)>| {
            if item.id.is_empty() {
                return;
            }
            let name = item.name.to_lowercase();
            let id = item.id.to_lowercase();
            if !query.is_empty() && !name.contains(&query) && !id.contains(&query) {
                return;
            }
            if !seen.insert(item.id.clone()) {
                return;
            }
            offered.push(Offered { item, file });
        };

        if kind == CatalogKind::Emoji {
            for raw in &lists.favorite_emojis {
                if let Some((item, file)) = offer_emoji(&state, raw, FAVORITES, everywhere, space) {
                    remember(&mut offered, &mut seen, item, file);
                }
            }
            let mut scored = lists.emoji_frecency.clone();
            scored.sort_by(|left, right| right.1.cmp(&left.1));
            let mut added = 0;
            for (raw, _) in scored {
                if added >= EMOJI_FREQUENT {
                    break;
                }
                if lists.favorite_emojis.iter().any(|held| same_emoji(held, &raw)) {
                    continue;
                }
                if let Some((item, file)) = offer_emoji(&state, &raw, FREQUENT, everywhere, space) {
                    let before = offered.len();
                    remember(&mut offered, &mut seen, item, file);
                    if offered.len() > before {
                        added += 1;
                    }
                }
            }
            for (guild, info) in &guilds {
                for emoji in &info.emojis {
                    if !emoji_allowed(info, emoji, everywhere, space, guild.as_str()) {
                        continue;
                    }
                    let (file, url) = model::emoji(&emoji.id, emoji.animated);
                    remember(
                        &mut offered,
                        &mut seen,
                        CatalogItem {
                            set: String::new(),
                            locked: false,
                            id: file.id.clone(),
                            name: format!(":{}:", emoji.name),
                            file: Some(file.clone()),
                            pack: info.name.clone(),
                            animated: emoji.animated,
                        },
                        Some((file, url)),
                    );
                }
            }
        } else {
            for id in &lists.favorite_stickers {
                if let Some((item, file)) = offer_sticker(&state, id, Some(FAVORITES), everywhere, space) {
                    remember(&mut offered, &mut seen, item, file);
                }
            }
            let mut scored = lists.sticker_frecency.clone();
            scored.sort_by(|left, right| right.1.cmp(&left.1));
            let mut added = 0;
            for (id, _) in scored {
                if added >= STICKER_FREQUENT {
                    break;
                }
                if lists.favorite_stickers.iter().any(|held| held == &id) {
                    continue;
                }
                if let Some((item, file)) = offer_sticker(&state, &id, Some(FREQUENT), everywhere, space) {
                    let before = offered.len();
                    remember(&mut offered, &mut seen, item, file);
                    if offered.len() > before {
                        added += 1;
                    }
                }
            }
            for (guild, info) in &guilds {
                if !everywhere && Some(guild.as_str()) != space {
                    continue;
                }
                for sticker in &info.stickers {
                    let Some((file, url)) = model::sticker(&sticker.id, sticker.format) else {
                        continue;
                    };
                    remember(
                        &mut offered,
                        &mut seen,
                        CatalogItem {
                            set: String::new(),
                            locked: false,
                            id: sticker.id.clone(),
                            name: sticker.name.clone(),
                            file: Some(file.clone()),
                            pack: info.name.clone(),
                            animated: sticker.animated,
                        },
                        Some((file, url)),
                    );
                }
            }
            if everywhere {
                for sticker in &state.standard_stickers {
                    let Some((file, url)) = model::sticker(&sticker.id, sticker.format) else {
                        continue;
                    };
                    remember(
                        &mut offered,
                        &mut seen,
                        CatalogItem {
                            set: String::new(),
                            locked: false,
                            id: sticker.id.clone(),
                            name: sticker.name.clone(),
                            file: Some(file.clone()),
                            pack: sticker.pack.clone(),
                            animated: sticker.format != 1,
                        },
                        Some((file, url)),
                    );
                }
            }
        }

        let more = offered.len() > from + PAGE;
        let mut items = Vec::new();
        let mut files = Vec::new();
        for row in offered.into_iter().skip(from).take(PAGE) {
            if let Some(file) = row.file {
                files.push(file);
            }
            items.push(row.item);
        }
        (items, files, more)
    }

    /// What reactions a channel takes from us, from what READY and its updates said, so asking
    /// costs Discord nothing. In a server: Read Message History to react at all and Add
    /// Reactions to start a new one (without it only existing ones can be added to), worked out
    /// from the kept channel as the tree is; a thread goes by its channel. Custom emoji: without
    /// Nitro only the server's own and none animated, none in a DM; with it any. A message holds
    /// at most 20 different reactions. Super reactions are offered only with Nitro.
    pub(super) fn reaction_rules(&self, channel: &str) -> ReactionRules {
        let nitro = matches!(crate::hub::lock(&self.state).premium, 1..=3);
        let mut rules = ReactionRules {
            custom: if nitro { EmojiScope::All } else { EmojiScope::Space },
            animated: nitro,
            distinct: 20,
            burst: nitro,
            ..ReactionRules::default()
        };
        let row = self.ctx.known(channel).ok().flatten();
        let thread = row.as_ref().is_some_and(|row| row.kind == protocol::ConversationKind::Thread);
        let Some(guild) = row.and_then(|row| row.space) else {
            if !nitro {
                rules.custom = EmojiScope::None;
            }
            return rules;
        };
        let me = self.me();
        let kept: Vec<model::Kept> = self.ctx.note(&format!("kept:{guild}")).and_then(|kept| serde_json::from_str(&kept).ok()).unwrap_or_default();
        let state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get(&guild) else {
            return rules;
        };
        // Threads are not kept; a thread is known by its channel.
        let parent = info.channels.iter().find(|(id, _)| thread && *id == model::number(channel)).map(|(_, parent)| parent.to_string());
        let wanted = |id: &str| id == channel || parent.as_deref() == Some(id);
        let found = kept.into_iter().filter(|kept| wanted(&kept.id)).min_by_key(|kept| kept.id != channel);
        // A channel not kept (READY not yet seen) is left to Discord to judge.
        let Some(found) = found else {
            return rules;
        };
        let allowed = info.permissions(&guild, &me, &found.channel());
        if allowed & model::READ_MESSAGE_HISTORY == 0 {
            return ReactionRules::off("You can't react in this channel.");
        }
        rules.join_only = allowed & model::ADD_REACTIONS == 0;
        rules.burst = nitro && !rules.join_only;
        rules
    }

    /// Discord's GIF search, which it proxies to its provider. Sent as the result's address,
    /// which Discord unfurls. Each cell is the provider's `tinygif` (about 220 px), so the grid
    /// moves for a few hundred kilobytes a page. Unverified against a real account: the routes,
    /// `media_format` and the GIF object are the community's documentation.
    fn gifs(&self, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Refusal> {
        let offset = cursor.and_then(|cursor| cursor.parse().ok()).unwrap_or(0);
        let asked = (self.ctx.account.account.clone(), query.trim().to_lowercase(), offset);
        if let Some((_, page)) = crate::hub::lock(&GIF_ANSWERS).get_or_insert_default().get(&asked).filter(|(at, _)| at.elapsed() < GIFS_KEPT) {
            return Ok(page.clone());
        }
        let page = self.gifs_asked(query, offset)?;
        let mut held = crate::hub::lock(&GIF_ANSWERS);
        let held = held.get_or_insert_default();
        held.retain(|_, (at, _)| at.elapsed() < GIFS_KEPT);
        if held.len() >= GIFS_HELD {
            if let Some(oldest) = held.iter().min_by_key(|(_, (at, _))| *at).map(|(key, _)| key.clone()) {
                held.remove(&oldest);
            }
        }
        held.insert(asked, (Instant::now(), page.clone()));
        Ok(page)
    }

    fn gifs_asked(&self, query: &str, offset: u32) -> Result<CatalogPage, Refusal> {
        let trimmed = query.trim();
        let (mut items, mut files) = (Vec::new(), Vec::new());
        let mut seen = HashSet::new();
        // Favourites and category tiles lead the first page only. A search puts hits first so
        // the result the shell sends is still the first GIF, and suggested terms follow it.
        if trimmed.is_empty() && offset == 0 {
            self.ensure_frecency();
            let favourites = {
                let state = crate::hub::lock(&self.state);
                super::proto::decode_frecency(&state.frecency).favorite_gifs
            };
            for gif in favourites {
                push_favourite(&mut items, &mut files, &mut seen, &gif);
            }
            if let Ok(body) = self.call("GET", "/gifs/trending", None) {
                for (name, src) in categories_of(&body) {
                    push_category(&mut items, &mut files, &mut seen, &name, &src);
                }
            }
        }
        let path = match trimmed {
            "" if offset == 0 => format!("/gifs/trending-gifs?media_format=tinygif&locale=en-US&limit={GIFS}"),
            "" => format!("/gifs/trending-gifs?media_format=tinygif&locale=en-US&limit={GIFS}&offset={offset}"),
            query => format!("/gifs/search?q={}&media_format=tinygif&locale=en-US&limit={GIFS}&offset={offset}", escaped(query)),
        };
        let answer = self.call("GET", &path, None).map_err(refusal)?;
        let gifs = gif_list(&answer)?;
        let full = gifs.len() == GIFS as usize;
        let pack = if trimmed.is_empty() { TRENDING } else { "" };
        for gif in gifs.into_iter().filter(|gif| !gif.url.is_empty() && gif.shown().starts_with("http")) {
            if !seen.insert(gif.url.clone()) {
                continue;
            }
            remember_gif(&gif.url, &gif.src, gif.width, gif.height);
            let shown = gif.shown().to_owned();
            let file = MediaFile {
                width: gif.width,
                height: gif.height,
                ..MediaFile::new(format!("gif:{:x}", fnv(&shown)), MediaKind::Image)
            };
            files.push((file.clone(), shown));
            items.push(CatalogItem {
                set: String::new(),
                locked: false,
                id: gif.url,
                name: gif.title,
                file: Some(file),
                pack: pack.to_owned(),
                animated: true,
            });
        }
        if !trimmed.is_empty() && offset == 0 {
            let suggest = format!("/gifs/suggest?q={}&locale=en-US", escaped(trimmed));
            if let Ok(body) = self.call("GET", &suggest, None) {
                for term in suggestions_of(&body) {
                    let id = format!("cat:{term}");
                    if !seen.insert(id.clone()) {
                        continue;
                    }
                    items.push(CatalogItem { id, name: term, file: None, pack: SUGGESTIONS.to_owned(), animated: false, set: String::new(), locked: false });
                }
            }
        }
        self.ctx.media(&files).map_err(|err| Refusal::Refused(err.to_string()))?;
        Ok(CatalogPage { items, cursor: full.then(|| (offset + GIFS).to_string()) })
    }
}

fn remember_gif(url: &str, src: &str, width: u32, height: u32) {
    if url.is_empty() {
        return;
    }
    let mut held = crate::hub::lock(&GIF_FACTS);
    let held = held.get_or_insert_default();
    if held.len() >= 256 && !held.contains_key(url) {
        if let Some(drop) = held.keys().next().cloned() {
            held.remove(&drop);
        }
    }
    held.insert(url.to_owned(), GifFact { src: src.to_owned(), width, height, format: gif_format(src) });
}

fn gif_format(src: &str) -> u32 {
    let path = src.split('?').next().unwrap_or("");
    if path.ends_with(".mp4") || path.ends_with(".webm") {
        2
    } else if src.is_empty() {
        0
    } else {
        1
    }
}

fn push_favourite(items: &mut Vec<CatalogItem>, files: &mut Vec<(MediaFile, String)>, seen: &mut HashSet<String>, gif: &super::proto::FavoriteGif) {
    if gif.url.is_empty() || !seen.insert(gif.url.clone()) {
        return;
    }
    let direct = {
        let path = gif.src.split('?').next().unwrap_or("");
        (gif.src.starts_with("https://") || gif.src.starts_with("http://"))
            && (path.ends_with(".gif") || path.ends_with(".webp") || path.ends_with(".png") || path.ends_with(".jpg") || path.ends_with(".jpeg"))
    };
    let file = direct.then(|| {
        let file = MediaFile {
            width: gif.width,
            height: gif.height,
            ..MediaFile::new(format!("gif:{:x}", fnv(&gif.src)), MediaKind::Image)
        };
        (file, gif.src.clone())
    });
    if let Some((file, url)) = &file {
        files.push((file.clone(), url.clone()));
    }
    items.push(CatalogItem {
        set: String::new(),
        locked: false,
        id: gif.url.clone(),
        name: "GIF".to_owned(),
        file: file.map(|(file, _)| file),
        pack: FAVORITES.to_owned(),
        animated: true,
    });
}

fn push_category(items: &mut Vec<CatalogItem>, files: &mut Vec<(MediaFile, String)>, seen: &mut HashSet<String>, name: &str, src: &str) {
    let id = format!("cat:{name}");
    if !seen.insert(id.clone()) {
        return;
    }
    let direct = (src.starts_with("https://") || src.starts_with("http://")).then_some(src);
    let file = direct.map(|src| {
        let file = MediaFile::new(format!("gifcat:{:x}", fnv(src)), MediaKind::Image);
        files.push((file.clone(), src.to_owned()));
        file
    });
    items.push(CatalogItem { id, name: name.to_owned(), file, pack: CATEGORIES.to_owned(), animated: false, set: String::new(), locked: false });
}

fn gif_list(body: &str) -> Result<Vec<Gif>, Refusal> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|err| Refusal::Refused(err.to_string()))?;
    let list = if value.is_array() {
        value
    } else if let Some(gifs) = value.get("gifs").filter(|gifs| gifs.is_array()) {
        gifs.clone()
    } else {
        return Ok(Vec::new());
    };
    serde_json::from_value(list).map_err(|err| Refusal::Refused(err.to_string()))
}

fn categories_of(body: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(list) = value.get("categories").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| {
            let name = item.get("name").and_then(|value| value.as_str()).unwrap_or("").trim();
            if name.is_empty() {
                return None;
            }
            let src = item.get("src").and_then(|value| value.as_str()).unwrap_or("").to_owned();
            Some((name.to_owned(), src))
        })
        .collect()
}

/// Suggested search terms. A GIF object (it has `url`) is not a term: those belong in the hits.
fn suggestions_of(body: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let list = value
        .as_array()
        .cloned()
        .or_else(|| value.get("suggestions").and_then(|value| value.as_array()).cloned())
        .unwrap_or_default();
    list.into_iter()
        .filter_map(|item| {
            if item.get("url").is_some() {
                return None;
            }
            item.as_str()
                .map(str::to_owned)
                .or_else(|| item.get("term").or_else(|| item.get("text")).or_else(|| item.get("name")).and_then(|value| value.as_str()).map(str::to_owned))
        })
        .filter(|term| !term.is_empty() && !term.contains(['\n', '\r']))
        .take(8)
        .collect()
}

struct Offered {
    item: CatalogItem,
    file: Option<(MediaFile, String)>,
}

enum EmojiToken {
    Unicode(String),
    Custom { animated: bool, name: String, id: String },
}

/// `emoji:{id}`, `a:name:id`, `name:id`, a bare snowflake, or a Unicode glyph.
fn emoji_token(raw: &str) -> EmojiToken {
    let text = raw.trim().trim_matches(':');
    if let Some(rest) = text.strip_prefix("emoji:") {
        let id = rest.split(':').next().unwrap_or(rest);
        if !id.is_empty() && id.chars().all(|ch| ch.is_ascii_digit()) {
            return EmojiToken::Custom { animated: false, name: String::new(), id: id.to_owned() };
        }
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() == 3 && parts[0] == "a" && !parts[2].is_empty() && parts[2].chars().all(|ch| ch.is_ascii_digit()) {
        return EmojiToken::Custom { animated: true, name: parts[1].to_owned(), id: parts[2].to_owned() };
    }
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() && parts[1].chars().all(|ch| ch.is_ascii_digit()) {
        return EmojiToken::Custom { animated: false, name: parts[0].to_owned(), id: parts[1].to_owned() };
    }
    if !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit()) {
        return EmojiToken::Custom { animated: false, name: String::new(), id: text.to_owned() };
    }
    EmojiToken::Unicode(text.to_owned())
}

fn emoji_allowed(info: &super::GuildInfo, emoji: &Pickable, everywhere: bool, space: Option<&str>, guild: &str) -> bool {
    if !everywhere && Some(guild) != space {
        return false;
    }
    // An animated emoji is Nitro's, even in its own server.
    if !everywhere && emoji.animated {
        return false;
    }
    emoji.roles.is_empty() || emoji.roles.iter().any(|role| info.my_roles.contains(role))
}

fn same_emoji(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (emoji_token(left), emoji_token(right)) {
        (EmojiToken::Custom { id: left, .. }, EmojiToken::Custom { id: right, .. }) => !left.is_empty() && left == right,
        (EmojiToken::Unicode(left), EmojiToken::Unicode(right)) => !left.is_empty() && left == right,
        _ => false,
    }
}

fn offer_emoji(
    state: &super::State,
    raw: &str,
    pack: &str,
    everywhere: bool,
    space: Option<&str>,
) -> Option<(CatalogItem, Option<(MediaFile, String)>)> {
    match emoji_token(raw) {
        EmojiToken::Unicode(glyph) => {
            if glyph.is_empty() {
                return None;
            }
            Some((
                CatalogItem { id: glyph.clone(), name: glyph, file: None, pack: pack.to_owned(), animated: false, set: String::new(), locked: false },
                None,
            ))
        }
        EmojiToken::Custom { animated, name, id } => {
            let found = state.guilds.iter().find_map(|(guild, info)| {
                info.emojis.iter().find(|emoji| emoji.id == id).map(|emoji| (guild.clone(), emoji.animated, emoji.name.clone(), emoji.roles.clone(), info.my_roles.clone()))
            });
            if let Some((guild, animated, emoji_name, roles, my_roles)) = found {
                let stand_in = Pickable { id: id.clone(), name: emoji_name.clone(), animated, format: 0, roles };
                // Role check needs the guild's membership. Rebuild the small check without holding the emoji borrow.
                let allowed = stand_in.roles.is_empty() || stand_in.roles.iter().any(|role| my_roles.contains(role));
                let in_space = everywhere || Some(guild.as_str()) == space;
                if !in_space || (!everywhere && stand_in.animated) || !allowed {
                    return None;
                }
                let (file, url) = model::emoji(&id, animated);
                return Some((
                    CatalogItem {
                        set: String::new(),
                        locked: false,
                        id: file.id.clone(),
                        name: format!(":{emoji_name}:"),
                        file: Some(file.clone()),
                        pack: pack.to_owned(),
                        animated,
                    },
                    Some((file, url)),
                ));
            }
            // With Nitro an external emoji still has a public picture. Without it, hide it.
            if !everywhere {
                return None;
            }
            let (file, url) = model::emoji(&id, animated);
            let shown = if name.is_empty() { id } else { name };
            Some((
                CatalogItem {
                    set: String::new(),
                    locked: false,
                    id: file.id.clone(),
                    name: format!(":{shown}:"),
                    file: Some(file.clone()),
                    pack: pack.to_owned(),
                    animated,
                },
                Some((file, url)),
            ))
        }
    }
}

fn offer_sticker(
    state: &super::State,
    id: &str,
    pack: Option<&str>,
    everywhere: bool,
    space: Option<&str>,
) -> Option<(CatalogItem, Option<(MediaFile, String)>)> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    if let Some((guild, name, pack_name, format, animated)) = state.guilds.iter().find_map(|(guild, info)| {
        info.stickers.iter().find(|sticker| sticker.id == id).map(|sticker| {
            (guild.clone(), sticker.name.clone(), info.name.clone(), sticker.format, sticker.animated)
        })
    }) {
        if !everywhere && Some(guild.as_str()) != space {
            return None;
        }
        let (file, url) = model::sticker(id, format)?;
        return Some((
            CatalogItem {
                set: String::new(),
                locked: false,
                id: id.to_owned(),
                name,
                file: Some(file.clone()),
                pack: pack.unwrap_or(&pack_name).to_owned(),
                animated,
            },
            Some((file, url)),
        ));
    }
    if !everywhere {
        return None;
    }
    let packed = state.standard_stickers.iter().find(|sticker| sticker.id == id)?;
    let (file, url) = model::sticker(&packed.id, packed.format)?;
    Some((
        CatalogItem {
            set: String::new(),
            locked: false,
            id: packed.id.clone(),
            name: packed.name.clone(),
            file: Some(file.clone()),
            pack: pack.unwrap_or(&packed.pack).to_owned(),
            animated: packed.format != 1,
        },
        Some((file, url)),
    ))
}

fn frecency_bytes(body: &str) -> Option<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    match value.get("settings").and_then(|settings| settings.as_str()) {
        None | Some("") => Some(Vec::new()),
        Some(settings) => super::proto::decode_settings(settings).or(Some(Vec::new())),
    }
}

fn parse_sticker_packs(body: &str) -> Vec<super::PackedSticker> {
    #[derive(Deserialize)]
    struct Packs {
        #[serde(default)]
        sticker_packs: Vec<Pack>,
    }
    #[derive(Deserialize)]
    struct Pack {
        #[serde(default)]
        name: String,
        #[serde(default)]
        stickers: Vec<GuildSticker>,
    }
    let Ok(packs) = serde_json::from_str::<Packs>(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pack in packs.sticker_packs {
        for sticker in pack.stickers {
            if sticker.id.is_empty() || !sticker.available || model::sticker(&sticker.id, sticker.format_type).is_none() {
                continue;
            }
            out.push(super::PackedSticker {
                id: sticker.id,
                name: sticker.name,
                format: sticker.format_type,
                pack: pack.name.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_shows_the_small_moving_gif_and_falls_back_to_the_still() {
        let gif = |src: &str| Gif { url: "https://tenor.com/view/x".into(), title: String::new(), src: src.into(), preview: "https://media.tenor.com/x.png".into(), width: 220, height: 124 };
        assert_eq!(gif("https://media.tenor.com/xAAAM/cat.gif").shown(), "https://media.tenor.com/xAAAM/cat.gif");
        // An MP4 needs a video decoder the window does not have; nothing at all is no picture.
        assert_eq!(gif("https://media.tenor.com/xAAPo/cat.mp4").shown(), "https://media.tenor.com/x.png");
        assert_eq!(gif("").shown(), "https://media.tenor.com/x.png");
    }

    #[test]
    fn an_emoji_favourite_matches_its_catalog_id_and_its_stored_name() {
        assert!(same_emoji("emoji:5101", "ferris:5101"));
        assert!(same_emoji("a:party:5103", "5103"));
        assert!(!same_emoji("😀", "ferris:5101"));
        assert!(matches!(emoji_token("emoji:5101"), EmojiToken::Custom { id, animated: false, .. } if id == "5101"));
        assert!(matches!(emoji_token("a:party:5103"), EmojiToken::Custom { animated: true, id, .. } if id == "5103"));
        assert!(matches!(emoji_token("😀"), EmojiToken::Unicode(glyph) if glyph == "😀"));
    }

    #[test]
    fn categories_and_suggestions_keep_terms_and_skip_gif_objects() {
        let categories = categories_of(r#"{"categories":[{"name":"happy","src":"https://media.tenor.com/happy.gif"},{"name":"  ","src":"https://x"}]} "#);
        assert_eq!(categories, vec![("happy".to_owned(), "https://media.tenor.com/happy.gif".to_owned())]);
        assert!(categories_of("nope").is_empty());
        assert_eq!(suggestions_of(r#"["otter", {"term":"cat typing"}, {"url":"https://tenor.com/view/x","name":"skip"}]"#), vec!["otter".to_owned(), "cat typing".to_owned()]);
        assert!(suggestions_of("nope").is_empty());
        assert_eq!(gif_format("https://media.tenor.com/a.mp4?w=1"), 2);
        assert_eq!(gif_format("https://media.tenor.com/a.gif"), 1);
    }
}
