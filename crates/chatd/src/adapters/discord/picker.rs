//! The picker's catalogs: the servers' own emoji and stickers, kept from READY and their update
//! events, and Discord's GIF search, asked only when the user types (the shell debounces).

use super::super::{CatalogPage, Refusal};
use super::model::{self, GuildEmoji, GuildSticker};
use super::{refusal, Discord, Pickable};
use protocol::{CatalogItem, CatalogKind, EmojiScope, MediaFile, MediaKind, ReactionRules};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Items a page of a server's own emoji or stickers holds.
const PAGE: usize = protocol::CATALOG_LIMIT;
/// GIFs asked for at once: what a picker shows before it is scrolled.
const GIFS: u32 = 50;
/// A GIF search's answer is kept this long, so typing a query again, or opening the picker
/// again on trending, asks Discord nothing.
const GIFS_KEPT: Duration = Duration::from_secs(600);
/// GIF answers kept at most, across accounts; the oldest goes.
const GIFS_HELD: usize = 64;

/// GIF answers by account and query: when, and what.
type GifAnswers = HashMap<(String, String), (Instant, CatalogPage)>;
static GIF_ANSWERS: Mutex<Option<GifAnswers>> = Mutex::new(None);

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
    /// A still of it.
    #[serde(default)]
    preview: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
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

    /// One page of a picker's catalog. Emoji and stickers come from what READY said: without
    /// Nitro only the server the message is said in (none in a DM) and no animated emoji, with
    /// it every server's. An emoji limited to roles we lack is never offered. GIFs are Discord's
    /// own search (trending when nothing is typed).
    pub(super) fn catalog(&self, kind: CatalogKind, space: Option<&str>, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Refusal> {
        match kind {
            CatalogKind::Gifs => self.gifs(query),
            CatalogKind::Emoji | CatalogKind::Stickers => {
                let from: usize = cursor.and_then(|cursor| cursor.parse().ok()).unwrap_or(0);
                let (items, files, more) = self.pickables(kind, space, query, from);
                self.ctx.media(&files).map_err(|err| Refusal::Refused(err.to_string()))?;
                Ok(CatalogPage { items, cursor: more.then(|| (from + PAGE).to_string()) })
            }
        }
    }

    fn pickables(&self, kind: CatalogKind, space: Option<&str>, query: &str, from: usize) -> (Vec<CatalogItem>, Vec<(MediaFile, String)>, bool) {
        let state = crate::hub::lock(&self.state);
        let everywhere = matches!(state.premium, 1..=3);
        let mut guilds: Vec<(&String, &super::GuildInfo)> = state.guilds.iter().filter(|(id, _)| everywhere || Some(id.as_str()) == space).collect();
        // The server it is said in first, then the user's own order.
        guilds.sort_by_key(|(id, info)| (Some(id.as_str()) != space, info.position));
        let query = query.trim().trim_matches(':').to_lowercase();
        let all = guilds.into_iter().flat_map(|(_, info)| {
            let list = if kind == CatalogKind::Emoji { &info.emojis } else { &info.stickers };
            let usable = move |pickable: &&Pickable| {
                let emoji = kind == CatalogKind::Emoji;
                let allowed = pickable.roles.is_empty() || pickable.roles.iter().any(|role| info.my_roles.contains(role));
                // An animated emoji is Nitro's, even in its own server.
                !emoji || (allowed && (everywhere || !pickable.animated))
            };
            list.iter().filter(usable).map(move |pickable| (info.name.as_str(), pickable))
        });
        let mut matching = all.filter(|(_, pickable)| query.is_empty() || pickable.name.to_lowercase().contains(&query));
        let page: Vec<(&str, &Pickable)> = matching.by_ref().skip(from).take(PAGE).collect();
        let more = matching.next().is_some();
        let (mut items, mut files) = (Vec::with_capacity(page.len()), Vec::with_capacity(page.len()));
        for (pack, pickable) in page {
            let registered = match kind {
                CatalogKind::Emoji => Some(model::emoji(&pickable.id)),
                _ => model::sticker(&pickable.id, pickable.format),
            };
            let Some((file, url)) = registered else {
                continue;
            };
            items.push(CatalogItem {
                // A custom emoji is named by its file, as a reaction names it; a sticker by its id.
                id: if kind == CatalogKind::Emoji { file.id.clone() } else { pickable.id.clone() },
                name: if kind == CatalogKind::Emoji { format!(":{}:", pickable.name) } else { pickable.name.clone() },
                file: Some(file.clone()),
                pack: pack.to_owned(),
                animated: pickable.animated,
            });
            files.push((file, url));
        }
        (items, files, more)
    }

    /// What reactions a channel takes from us, from what READY and its updates said, so asking
    /// costs Discord nothing. In a server: Read Message History to react at all and Add
    /// Reactions to start a new one (without it only existing ones can be added to), worked out
    /// from the kept channel as the tree is; a thread goes by its channel. Custom emoji: without
    /// Nitro only the server's own and none animated, none in a DM; with it any. A message holds
    /// at most 20 different reactions. Super reactions are never sent.
    pub(super) fn reaction_rules(&self, channel: &str) -> ReactionRules {
        let nitro = matches!(crate::hub::lock(&self.state).premium, 1..=3);
        let mut rules = ReactionRules {
            custom: if nitro { EmojiScope::All } else { EmojiScope::Space },
            animated: nitro,
            distinct: 20,
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
        rules
    }

    /// Discord's GIF search, which it proxies to its provider. Sent as the result's address,
    /// which Discord unfurls. Unverified against a real account: the routes and the GIF object
    /// are the community's documentation.
    // CONTRACT: the stills are on the provider's host (`media.tenor.com`), which the media
    // cache does not trust for Discord (`blobs/fetch.rs` takes only Discord's CDN), so they are
    // registered and will not load until it does.
    fn gifs(&self, query: &str) -> Result<CatalogPage, Refusal> {
        let asked = (self.ctx.account.account.clone(), query.trim().to_lowercase());
        if let Some((_, page)) = crate::hub::lock(&GIF_ANSWERS).get_or_insert_default().get(&asked).filter(|(at, _)| at.elapsed() < GIFS_KEPT) {
            return Ok(page.clone());
        }
        let page = self.gifs_asked(query)?;
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

    fn gifs_asked(&self, query: &str) -> Result<CatalogPage, Refusal> {
        let path = match query.trim() {
            "" => format!("/gifs/trending-gifs?media_format=mp4&locale=en-US&limit={GIFS}"),
            query => format!("/gifs/search?q={}&media_format=mp4&locale=en-US&limit={GIFS}", escaped(query)),
        };
        let answer = self.call("GET", &path, None).map_err(refusal)?;
        let gifs: Vec<Gif> = serde_json::from_str(&answer).map_err(|err| Refusal::Refused(err.to_string()))?;
        let (mut items, mut files) = (Vec::new(), Vec::new());
        for gif in gifs.into_iter().filter(|gif| !gif.url.is_empty() && gif.preview.starts_with("http")) {
            let file = MediaFile {
                width: gif.width,
                height: gif.height,
                ..MediaFile::new(format!("gif:{:x}", fnv(&gif.preview)), MediaKind::Image)
            };
            files.push((file.clone(), gif.preview));
            items.push(CatalogItem {
                id: gif.url,
                name: gif.title,
                file: Some(file),
                pack: String::new(),
                animated: true,
            });
        }
        self.ctx.media(&files).map_err(|err| Refusal::Refused(err.to_string()))?;
        Ok(CatalogPage { items, cursor: None })
    }
}
