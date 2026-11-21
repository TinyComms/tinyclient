//! Teams composer GIF (Giphy) and sticker/meme catalog (`Command::Catalog`).
//!
//! GIFs go through Teams' own middle-tier Giphy proxy (`/beta/giphy/trending` and
//! `/beta/giphy/search`) so tenant Giphy policy and content ratings are honoured.
//! Stickers and meme packs come from `/beta/stickers` on the middle tier. Preview images
//! are registered with chatd's media store; `Command::Download` never attaches Teams
//! credentials to third-party Giphy URLs.

use super::cloud::TeamsCloud;
use super::{escaped, html, Audience, Auth, Body, Fault, Teams};
use crate::adapters::CatalogPage;
use protocol::{CatalogItem, CatalogKind, MediaFile, MediaKind};
use serde_json::Value;

/// Maximum cached catalog dimensions/titles kept in memory for outgoing picks.
const META_CAP: usize = 512;

/// FNV-1a 64-bit hash for deterministic catalog media ids.
pub(super) fn fnv(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn dim(value: &Value, fallback: u32) -> u32 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        .filter(|&n| n > 0)
        .map(|n| n.min(u64::from(u32::MAX)) as u32)
        .unwrap_or(fallback)
}

impl Teams {
    pub(super) fn catalog(&self, kind: CatalogKind, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Fault> {
        match kind {
            CatalogKind::Gifs => self.gif_catalog(query, cursor),
            CatalogKind::Stickers => self.sticker_catalog(query, cursor),
            CatalogKind::Emoji => Ok(CatalogPage::default()),
        }
    }

    fn remember_meta(&self, key: String, width: u32, height: u32, name: String) {
        let mut state = self.state();
        if state.catalog_meta.len() >= META_CAP {
            state.catalog_meta.clear();
        }
        state.catalog_meta.insert(key, (width, height, name));
    }

    /// Trending or searched GIFs from Teams' middle-tier Giphy endpoint.
    fn gif_catalog(&self, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Fault> {
        if self.state().giphy_disabled || self.cloud != TeamsCloud::Worldwide {
            return Err(Fault::Refused {
                status: 403,
                message: "GIFs are disabled by your Teams organization.".to_owned(),
            });
        }
        let place = self.place()?;
        let q = query.trim();
        let mut url = if q.is_empty() {
            format!("{}/beta/giphy/trending?limit=24", place.middle)
        } else {
            format!("{}/beta/giphy/search?q={}&limit=24", place.middle, escaped(q))
        };
        if let Some(c) = cursor.map(str::trim).filter(|s| !s.is_empty()) {
            url.push_str("&offset=");
            url.push_str(&escaped(c));
        }
        let answer = self.call("GET", &url, Auth::Both(Audience::Spaces), Body::None, false)?;
        let resp = &answer.body;
        let entries = resp["data"]
            .as_array()
            .or_else(|| resp["results"].as_array())
            .or_else(|| resp["gifs"].as_array())
            .map_or(&[][..], Vec::as_slice);
        let mut items = Vec::with_capacity(entries.len());
        let mut files = Vec::new();
        let gif_allowed = |u: &str| html::giphy(u) || self.ours(u);
        for entry in entries {
            let gif_url = entry["images"]["original"]["url"]
                .as_str()
                .or_else(|| entry["url"].as_str())
                .or_else(|| entry["media_url"].as_str())
                .map(str::trim)
                .filter(|u| gif_allowed(u));
            let Some(gif_url) = gif_url else {
                continue;
            };
            let preview_url = entry["images"]["fixed_height_small"]["url"]
                .as_str()
                .or_else(|| entry["images"]["fixed_width_small"]["url"].as_str())
                .or_else(|| entry["preview_url"].as_str())
                .or_else(|| entry["thumb_url"].as_str())
                .map(str::trim)
                .filter(|u| gif_allowed(u))
                .unwrap_or(gif_url);
            let width = dim(&entry["images"]["original"]["width"], dim(&entry["width"], 240));
            let height = dim(&entry["images"]["original"]["height"], dim(&entry["height"], 180));
            let preview_w = dim(&entry["images"]["fixed_height_small"]["width"], width);
            let preview_h = dim(&entry["images"]["fixed_height_small"]["height"], height);
            let name = entry["title"]
                .as_str()
                .or_else(|| entry["name"].as_str())
                .unwrap_or("")
                .trim()
                .to_owned();
            let pack = entry["pack"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| if q.is_empty() { "Trending".to_owned() } else { String::new() });
            let file = MediaFile {
                width: preview_w,
                height: preview_h,
                ..MediaFile::new(format!("gif:{:x}", fnv(preview_url)), MediaKind::Image)
            };
            files.push((file.clone(), preview_url.to_owned()));
            self.remember_meta(gif_url.to_owned(), width, height, name.clone());
            items.push(CatalogItem {
                id: gif_url.to_owned(),
                name,
                file: Some(file),
                pack,
                animated: true,
            });
        }
        if !files.is_empty() {
            self.ctx.media(&files).map_err(|err| Fault::Net(err.to_string()))?;
        }
        let next_cursor = next_gif_cursor(resp);
        Ok(CatalogPage { items, cursor: next_cursor })
    }

    /// Teams sticker and meme packs from the middle tier.
    fn sticker_catalog(&self, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Fault> {
        if self.state().stickers_disabled {
            return Err(Fault::Refused {
                status: 403,
                message: "Stickers are disabled by your Teams organization.".to_owned(),
            });
        }
        let place = self.place()?;
        let q = query.trim();
        let mut url = format!("{}/beta/stickers", place.middle);
        let mut sep = '?';
        if !q.is_empty() {
            url.push(sep);
            url.push_str("q=");
            url.push_str(&escaped(q));
            sep = '&';
        }
        if let Some(c) = cursor.map(str::trim).filter(|s| !s.is_empty()) {
            url.push(sep);
            url.push_str("cursor=");
            url.push_str(&escaped(c));
        }
        let answer = self.call("GET", &url, Auth::Both(Audience::Spaces), Body::None, false)?;
        let resp = &answer.body;
        let q_lower = q.to_ascii_lowercase();
        let mut items = Vec::new();
        let mut files = Vec::new();
        let allowed = |u: &str| self.pictures(u) || self.ours(u);

        let mut push_entry = |entry: &Value, default_pack: &str| {
            let sticker_url = entry["imageUrl"]
                .as_str()
                .or_else(|| entry["url"].as_str())
                .or_else(|| entry["src"].as_str())
                .map(str::trim)
                .filter(|u| allowed(u));
            let Some(sticker_url) = sticker_url else {
                return;
            };
            let name = entry["name"]
                .as_str()
                .or_else(|| entry["title"].as_str())
                .or_else(|| entry["alt"].as_str())
                .unwrap_or("Sticker")
                .trim()
                .to_owned();
            let pack = entry["pack"]
                .as_str()
                .or_else(|| entry["packName"].as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(default_pack)
                .to_owned();
            if !q_lower.is_empty()
                && !name.to_ascii_lowercase().contains(&q_lower)
                && !pack.to_ascii_lowercase().contains(&q_lower)
            {
                return;
            }
            let width = dim(&entry["width"], 180);
            let height = dim(&entry["height"], 180);
            let animated = entry["animated"].as_bool().unwrap_or(false)
                || sticker_url.to_ascii_lowercase().ends_with(".gif");
            let file = MediaFile {
                width,
                height,
                ..MediaFile::new(format!("sticker:{:x}", fnv(sticker_url)), MediaKind::Image)
            };
            files.push((file.clone(), sticker_url.to_owned()));
            self.remember_meta(sticker_url.to_owned(), width, height, name.clone());
            items.push(CatalogItem {
                id: sticker_url.to_owned(),
                name,
                file: Some(file),
                pack,
                animated,
            });
        };

        if let Some(packs) = resp["packs"].as_array() {
            for pack in packs {
                let pack_name = pack["name"]
                    .as_str()
                    .or_else(|| pack["title"].as_str())
                    .unwrap_or("Stickers")
                    .trim();
                let list = pack["stickers"]
                    .as_array()
                    .or_else(|| pack["items"].as_array())
                    .map_or(&[][..], Vec::as_slice);
                for entry in list {
                    push_entry(entry, pack_name);
                }
            }
        } else {
            let list = resp["stickers"]
                .as_array()
                .or_else(|| resp["items"].as_array())
                .or_else(|| resp["data"].as_array())
                .map_or(&[][..], Vec::as_slice);
            for entry in list {
                push_entry(entry, "Stickers");
            }
        }

        if !files.is_empty() {
            self.ctx.media(&files).map_err(|err| Fault::Net(err.to_string()))?;
        }
        let next_cursor = resp["nextCursor"]
            .as_str()
            .or_else(|| resp["next_cursor"].as_str())
            .or_else(|| resp["cursor"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        Ok(CatalogPage { items, cursor: next_cursor })
    }
}

fn next_gif_cursor(resp: &Value) -> Option<String> {
    if let Some(c) = resp["next_cursor"]
        .as_str()
        .or_else(|| resp["next"].as_str())
        .or_else(|| resp["nextCursor"].as_str())
        .filter(|s| !s.is_empty())
    {
        return Some(c.to_owned());
    }
    let page = &resp["pagination"];
    let offset = page["offset"].as_u64()?;
    let count = page["count"].as_u64()?;
    let total = page["total_count"].as_u64()?;
    let next = offset.saturating_add(count);
    (count > 0 && next < total).then(|| next.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gif_cursor_advances_from_giphy_pagination() {
        let paged = json!({"pagination": {"offset": 0, "count": 24, "total_count": 100}});
        assert_eq!(next_gif_cursor(&paged).as_deref(), Some("24"));
        let done = json!({"pagination": {"offset": 80, "count": 20, "total_count": 100}});
        assert_eq!(next_gif_cursor(&done), None);
        let named = json!({"next_cursor": "abc123"});
        assert_eq!(next_gif_cursor(&named).as_deref(), Some("abc123"));
    }

    #[test]
    fn dim_reads_numbers_strings_and_defaults() {
        assert_eq!(dim(&json!(320), 240), 320);
        assert_eq!(dim(&json!("480"), 240), 480);
        assert_eq!(dim(&json!(0), 240), 240);
        assert_eq!(dim(&Value::Null, 180), 180);
    }
}
