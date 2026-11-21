//! Adaptive Cards and legacy bot cards (hero, thumbnail, Office 365 connector) into the shared
//! [`Card`] model.
//!
//! Security boundary: card images may point anywhere on the internet. Only images on Teams' own
//! media hosts (`hosted(url) == true`) are registered for fetching; external images are rendered as
//! labelled links and never fetched by chatd.
//!
//! Resilience: card payloads vary widely across bots and connectors. A malformed or empty card
//! returns nothing so the caller falls back to the one-line unsupported form, never a blank row.

use base64::Engine;
use protocol::{Card, CardAction, MediaFile, MediaKind, CARD_ACTIONS, CARD_FIELDS};
use serde_json::Value;

/// Teams card content types we parse into [`Card`].
const ADAPTIVE: &str = "application/vnd.microsoft.card.adaptive";
const HERO: &str = "application/vnd.microsoft.card.hero";
const THUMBNAIL: &str = "application/vnd.microsoft.card.thumbnail";
const O365_CONNECTOR: &str = "application/vnd.microsoft.teams.card.o365connector";

/// Parsed cards and any Teams-hosted media registered for them.
#[derive(Debug, Default)]
pub(super) struct ParsedCards {
    pub cards: Vec<Card>,
    pub media: Vec<(MediaFile, String)>,
}

/// Parses `properties.cards` (or an embedded `<Swift b64="...">` payload when `cards` is empty)
/// into structured [`Card`] entries. Returns an empty list if nothing meaningful could be read.
pub(super) fn parse_cards(
    cards: &[Value],
    content_html: &str,
    message_id: &str,
    message_link: &str,
    hosted: impl Fn(&str) -> bool,
) -> ParsedCards {
    let mut out = ParsedCards::default();
    let owned_swift;
    let source: &[Value] = if !cards.is_empty() {
        cards
    } else {
        owned_swift = swift_attachments(content_html);
        &owned_swift
    };
    let mut media_index = 0usize;
    for entry in source {
        if let Some(card) = parse_one(entry, message_id, message_link, &hosted, &mut out.media, &mut media_index) {
            out.cards.push(card);
        }
    }
    out
}

/// Extracts card attachments from `<Swift b64="...">` in legacy Teams `RichText/Media_Card` HTML.
fn swift_attachments(html: &str) -> Vec<Value> {
    let Some(start) = html.find("b64=\"") else {
        return Vec::new();
    };
    let rest = &html[start + 5..];
    let Some(end) = rest.find('"') else {
        return Vec::new();
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&rest[..end]) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    parsed["attachments"].as_array().cloned().unwrap_or_default()
}

fn parse_one(
    entry: &Value,
    message_id: &str,
    message_link: &str,
    hosted: &dyn Fn(&str) -> bool,
    media: &mut Vec<(MediaFile, String)>,
    media_index: &mut usize,
) -> Option<Card> {
    let content_type = entry["contentType"].as_str().unwrap_or("").to_ascii_lowercase();
    let content = match entry.get("content") {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).ok()?,
        Some(Value::Object(_)) => entry["content"].clone(),
        _ if entry.is_object() && (entry["type"] == "AdaptiveCard" || entry["@type"] == "MessageCard") => entry.clone(),
        _ => return None,
    };
    if !content.is_object() {
        return None;
    }
    let mut card = Card::default();
    let is_adaptive = content_type == ADAPTIVE
        || content["type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("AdaptiveCard"));
    let is_o365 = content_type == O365_CONNECTOR
        || content["@type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("MessageCard"));
    let is_hero = content_type == HERO || content_type == THUMBNAIL;

    if is_adaptive {
        parse_adaptive(&content, message_id, message_link, hosted, media, media_index, &mut card);
    } else if is_o365 {
        parse_o365(&content, message_id, message_link, hosted, media, media_index, &mut card);
    } else if is_hero {
        parse_hero(
            &content,
            content_type == THUMBNAIL,
            message_id,
            message_link,
            hosted,
            media,
            media_index,
            &mut card,
        );
    } else {
        return None;
    }
    card.bound();
    has_content(&card).then_some(card)
}

fn has_content(card: &Card) -> bool {
    !card.author.is_empty()
        || !card.title.is_empty()
        || !card.text.is_empty()
        || !card.fields.is_empty()
        || !card.actions.is_empty()
        || !card.footer.is_empty()
        || card.image.is_some()
        || card.thumbnail.is_some()
}

fn safe_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let ok = ["https://", "http://", "mailto:", "msteams:"]
        .iter()
        .any(|scheme| trimmed.get(..scheme.len()).is_some_and(|start| start.eq_ignore_ascii_case(scheme)));
    (ok && trimmed.len() > 8).then(|| trimmed.to_owned())
}

fn push_line(text: &mut String, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(trimmed);
}

fn push_action(card: &mut Card, label: &str, url: &str) {
    if card.actions.len() >= CARD_ACTIONS {
        return;
    }
    let Some(url) = safe_url(url) else {
        return;
    };
    let label = label.trim();
    let label = if label.is_empty() { url.as_str() } else { label };
    if card.actions.iter().any(|existing| existing.label == label && existing.url == url) {
        return;
    }
    card.actions.push(CardAction { label: label.to_owned(), url });
}

fn push_open_in_teams(card: &mut Card, title: &str, message_link: &str) {
    let title = title.trim();
    let label = if title.is_empty() {
        "Open in Teams".to_owned()
    } else {
        format!("{title} · Open in Teams")
    };
    push_action(card, &label, message_link);
}

fn push_fact(card: &mut Card, label: &str, value: &str) {
    let (label, value) = (label.trim(), value.trim());
    if label.is_empty() && value.is_empty() {
        return;
    }
    if card.fields.len() < CARD_FIELDS {
        card.fields.push((label.to_owned(), value.to_owned()));
    } else if label.is_empty() {
        push_line(&mut card.text, value);
    } else if value.is_empty() {
        push_line(&mut card.text, label);
    } else {
        push_line(&mut card.text, &format!("{label}: {value}"));
    }
}

fn add_image(
    card: &mut Card,
    url: &str,
    alt: &str,
    as_thumb: bool,
    message_id: &str,
    hosted: &dyn Fn(&str) -> bool,
    media: &mut Vec<(MediaFile, String)>,
    media_index: &mut usize,
) {
    let url = url.trim();
    if url.is_empty() {
        return;
    }
    if hosted(url) {
        let id = format!("cardimg:{message_id}:{}", *media_index);
        *media_index += 1;
        let file = MediaFile::new(id, MediaKind::Image);
        media.push((file.clone(), url.to_owned()));
        if as_thumb && card.thumbnail.is_none() {
            card.thumbnail = Some(file);
        } else if card.image.is_none() {
            card.image = Some(file);
        } else if card.thumbnail.is_none() {
            card.thumbnail = Some(file);
        }
    } else {
        // Never fetch external images outside Teams' hosts: show as a labelled link.
        let label = if alt.trim().is_empty() { "Image" } else { alt.trim() };
        push_action(card, label, url);
    }
}

/// Parses hex color like `"0076D7"` or `"#D93025"` into `0xRRGGBB`.
fn parse_hex_color(raw: &str) -> Option<u32> {
    let hex = raw.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

fn container_style_color(style: &str) -> Option<u32> {
    match style.to_ascii_lowercase().as_str() {
        "attention" => Some(0xD93025),
        "warning" => Some(0xF29900),
        "good" => Some(0x1E8E3E),
        "accent" => Some(0x5B5FC7),
        _ => None,
    }
}

/// Cleans Adaptive Card markdown subset (`**bold**`, `*italic*`, `` `code` ``, `[label](url)`,
/// headings, bullet lists) into readable plain text, collecting any markdown links as actions.
fn clean_ac_markdown(raw: &str, links: &mut Vec<(String, String)>) -> String {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    for (line_ix, line) in normalized.split('\n').enumerate() {
        if line_ix > 0 {
            out.push('\n');
        }
        // Strip leading markdown heading markers (`# `, `## `, etc.)
        let trimmed = line.trim_start();
        let line = if trimmed.starts_with('#') {
            let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
            if hashes <= 6 && trimmed.as_bytes().get(hashes) == Some(&b' ') {
                &trimmed[hashes + 1..]
            } else {
                line
            }
        } else {
            line
        };
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // Markdown link: [label](url)
            if bytes[i] == b'[' {
                if let Some(mid) = line[i + 1..].find("](") {
                    let label = &line[i + 1..i + 1 + mid];
                    let url_start = i + 1 + mid + 2;
                    if let Some(end) = line[url_start..].find(')') {
                        let url = line[url_start..url_start + end].trim();
                        if !label.is_empty() && safe_url(url).is_some() {
                            out.push_str(label);
                            links.push((label.trim().to_owned(), url.to_owned()));
                            i = url_start + end + 1;
                            continue;
                        }
                    }
                }
            }
            // Strip **, __, ~~, *, _, ` wrappers when paired on the same line
            if let Some(delim) = ["**", "__", "~~", "*", "_", "`"].iter().find(|d| line[i..].starts_with(**d)) {
                let dlen = delim.len();
                if let Some(close) = line[i + dlen..].find(*delim) {
                    if close > 0 {
                        let inner = &line[i + dlen..i + dlen + close];
                        if !inner.starts_with(' ') && !inner.ends_with(' ') {
                            out.push_str(inner);
                            i += dlen + close + dlen;
                            continue;
                        }
                    }
                }
            }
            let ch = line[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Strips simple HTML tags (used in legacy Hero / Connector cards) using the existing HTML parser.
fn clean_legacy_text(raw: &str, links: &mut Vec<(String, String)>) -> String {
    if raw.contains('<') && raw.contains('>') {
        let parsed = super::html::parse(raw, &super::people::Nobody, "");
        for span in &parsed.spans {
            if let protocol::Style::Link { url } = &span.style {
                let label = parsed
                    .text
                    .get(span.start as usize..span.end as usize)
                    .unwrap_or("")
                    .trim();
                if !label.is_empty() {
                    links.push((label.to_owned(), url.clone()));
                }
            }
        }
        return parsed.text;
    }
    clean_ac_markdown(raw, links)
}

// ---------------------------------------------------------------------------
// Adaptive Card
// ---------------------------------------------------------------------------

fn parse_adaptive(
    root: &Value,
    message_id: &str,
    message_link: &str,
    hosted: &dyn Fn(&str) -> bool,
    media: &mut Vec<(MediaFile, String)>,
    media_index: &mut usize,
    card: &mut Card,
) {
    if let Some(style) = root["style"].as_str() {
        card.color = card.color.or_else(|| container_style_color(style));
    }
    let mut links = Vec::new();
    if let Some(body) = root["body"].as_array() {
        walk_ac_elements(body, 0, message_id, message_link, hosted, media, media_index, card, &mut links);
    }
    if let Some(actions) = root["actions"].as_array() {
        parse_ac_actions(actions, message_link, card);
    }
    for (label, url) in links {
        push_action(card, &label, &url);
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_ac_elements(
    elements: &[Value],
    depth: u8,
    message_id: &str,
    message_link: &str,
    hosted: &dyn Fn(&str) -> bool,
    media: &mut Vec<(MediaFile, String)>,
    media_index: &mut usize,
    card: &mut Card,
    links: &mut Vec<(String, String)>,
) {
    if depth > 8 {
        return;
    }
    for el in elements {
        let kind = el["type"].as_str().unwrap_or("");
        match kind {
            "TextBlock" => {
                let raw = el["text"].as_str().unwrap_or("").trim();
                if raw.is_empty() {
                    continue;
                }
                let cleaned = clean_ac_markdown(raw, links);
                let cleaned = cleaned.trim();
                if cleaned.is_empty() {
                    continue;
                }
                let is_heading = el["weight"].as_str().is_some_and(|w| w.eq_ignore_ascii_case("bolder"))
                    || el["size"].as_str().is_some_and(|s| {
                        s.eq_ignore_ascii_case("large")
                            || s.eq_ignore_ascii_case("extraLarge")
                            || s.eq_ignore_ascii_case("medium")
                    })
                    || el["style"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("heading"));
                if card.color.is_none() {
                    if let Some(c) = el["color"].as_str().and_then(container_style_color) {
                        card.color = Some(c);
                    }
                }
                if card.title.is_empty() && card.text.is_empty() && (is_heading || !cleaned.contains('\n')) {
                    card.title = cleaned.to_owned();
                } else {
                    push_line(&mut card.text, cleaned);
                }
            }
            "RichTextBlock" => {
                if let Some(inlines) = el["inlines"].as_array() {
                    let mut line = String::new();
                    for inline in inlines {
                        match inline {
                            Value::String(s) => line.push_str(s),
                            Value::Object(_) => {
                                if let Some(s) = inline["text"].as_str() {
                                    line.push_str(s);
                                }
                                if let Some(action) = inline.get("selectAction") {
                                    if action["type"] == "Action.OpenUrl" {
                                        if let Some(url) = action["url"].as_str() {
                                            let label = inline["text"].as_str().unwrap_or(url);
                                            links.push((label.trim().to_owned(), url.to_owned()));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    push_line(&mut card.text, &line);
                }
            }
            "FactSet" => {
                for fact in el["facts"].as_array().into_iter().flatten() {
                    let title = fact["title"].as_str().unwrap_or("");
                    let value = fact["value"].as_str().unwrap_or("");
                    let clean_val = clean_ac_markdown(value, links);
                    push_fact(card, title, &clean_val);
                }
            }
            "Image" => {
                let url = el["url"].as_str().unwrap_or("");
                let alt = el["altText"].as_str().unwrap_or("");
                let small = el["size"].as_str().is_some_and(|s| {
                    s.eq_ignore_ascii_case("small") || s.eq_ignore_ascii_case("medium")
                });
                add_image(card, url, alt, small, message_id, hosted, media, media_index);
            }
            "ImageSet" => {
                for img in el["images"].as_array().into_iter().flatten() {
                    let url = img["url"].as_str().unwrap_or("");
                    let alt = img["altText"].as_str().unwrap_or("");
                    add_image(card, url, alt, true, message_id, hosted, media, media_index);
                }
            }
            "Container" => {
                if card.color.is_none() {
                    if let Some(c) = el["style"].as_str().and_then(container_style_color) {
                        card.color = Some(c);
                    }
                }
                if let Some(items) = el["items"].as_array() {
                    walk_ac_elements(items, depth + 1, message_id, message_link, hosted, media, media_index, card, links);
                }
                if let Some(action) = el.get("selectAction") {
                    parse_ac_actions(std::slice::from_ref(action), message_link, card);
                }
            }
            "ColumnSet" => {
                if let Some(columns) = el["columns"].as_array() {
                    // If every column holds a single short TextBlock, flatten as one compact line;
                    // otherwise walk each column's items in order.
                    let simple: Option<Vec<String>> = columns
                        .iter()
                        .map(|col| {
                            let items = col["items"].as_array()?;
                            if items.len() != 1 || items[0]["type"] != "TextBlock" {
                                return None;
                            }
                            let text = items[0]["text"].as_str()?.trim();
                            if text.is_empty() || text.contains('\n') || text.len() > 80 {
                                return None;
                            }
                            Some(clean_ac_markdown(text, &mut Vec::new()).trim().to_owned())
                        })
                        .collect();
                    if let Some(parts) = simple.filter(|p| p.len() >= 2) {
                        // Also capture any markdown links inside the simple columns.
                        for col in columns {
                            if let Some(t) = col["items"][0]["text"].as_str() {
                                clean_ac_markdown(t, links);
                            }
                        }
                        push_line(&mut card.text, &parts.join(" · "));
                    } else {
                        for col in columns {
                            if let Some(items) = col["items"].as_array() {
                                walk_ac_elements(items, depth + 1, message_id, message_link, hosted, media, media_index, card, links);
                            }
                        }
                    }
                }
            }
            "Table" => {
                for row in el["rows"].as_array().into_iter().flatten() {
                    let mut cells_text = Vec::new();
                    for cell in row["cells"].as_array().into_iter().flatten() {
                        let cell_str = flatten_inline_text(cell["items"].as_array().map_or(&[][..], Vec::as_slice), links);
                        cells_text.push(cell_str);
                    }
                    let line = cells_text.join(" | ");
                    if !line.trim().trim_matches('|').trim().is_empty() {
                        push_line(&mut card.text, &line);
                    }
                }
            }
            "ActionSet" => {
                if let Some(actions) = el["actions"].as_array() {
                    parse_ac_actions(actions, message_link, card);
                }
            }
            _ => {
                // Unknown container-like elements with `items`: walk gracefully.
                if let Some(items) = el["items"].as_array() {
                    walk_ac_elements(items, depth + 1, message_id, message_link, hosted, media, media_index, card, links);
                }
            }
        }
    }
}

fn flatten_inline_text(items: &[Value], links: &mut Vec<(String, String)>) -> String {
    let mut parts = Vec::new();
    for item in items {
        if let Some(t) = item["text"].as_str() {
            let cleaned = clean_ac_markdown(t, links);
            let trimmed = cleaned.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_owned());
            }
        } else if let Some(inner) = item["items"].as_array() {
            let sub = flatten_inline_text(inner, links);
            if !sub.is_empty() {
                parts.push(sub);
            }
        }
    }
    parts.join(" ")
}

fn parse_ac_actions(actions: &[Value], message_link: &str, card: &mut Card) {
    for action in actions {
        let kind = action["type"].as_str().unwrap_or("");
        let title = action["title"].as_str().unwrap_or("");
        match kind {
            "Action.OpenUrl" => {
                if let Some(url) = action["url"].as_str() {
                    push_action(card, title, url);
                }
            }
            "Action.Submit" | "Action.Execute" => {
                push_open_in_teams(card, title, message_link);
            }
            "Action.ShowCard" => {
                push_open_in_teams(card, title, message_link);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Hero & Thumbnail Cards
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn parse_hero(
    root: &Value,
    is_thumbnail: bool,
    message_id: &str,
    message_link: &str,
    hosted: &dyn Fn(&str) -> bool,
    media: &mut Vec<(MediaFile, String)>,
    media_index: &mut usize,
    card: &mut Card,
) {
    let mut links = Vec::new();
    if let Some(title) = root["title"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        card.title = clean_legacy_text(title, &mut links);
    }
    if let Some(sub) = root["subtitle"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let cleaned = clean_legacy_text(sub, &mut links);
        if card.author.is_empty() {
            card.author = cleaned;
        } else {
            push_line(&mut card.text, &cleaned);
        }
    }
    if let Some(text) = root["text"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let cleaned = clean_legacy_text(text, &mut links);
        push_line(&mut card.text, &cleaned);
    }
    if let Some(tap) = root.get("tap") {
        if tap["type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("openUrl")) {
            if let Some(url) = tap["value"].as_str().and_then(safe_url) {
                card.title_link = url;
            }
        }
    }
    for img in root["images"].as_array().into_iter().flatten() {
        let url = img["url"].as_str().unwrap_or("");
        let alt = img["alt"].as_str().unwrap_or("");
        add_image(card, url, alt, is_thumbnail, message_id, hosted, media, media_index);
    }
    for btn in root["buttons"].as_array().into_iter().flatten() {
        let kind = btn["type"].as_str().unwrap_or("");
        let title = btn["title"].as_str().unwrap_or("");
        if kind.eq_ignore_ascii_case("openUrl") {
            if let Some(url) = btn["value"].as_str() {
                push_action(card, title, url);
            }
        } else if !kind.is_empty() {
            push_open_in_teams(card, title, message_link);
        }
    }
    for (label, url) in links {
        push_action(card, &label, &url);
    }
}

// ---------------------------------------------------------------------------
// Office 365 Connector Card (MessageCard)
// ---------------------------------------------------------------------------

fn parse_o365(
    root: &Value,
    message_id: &str,
    message_link: &str,
    hosted: &dyn Fn(&str) -> bool,
    media: &mut Vec<(MediaFile, String)>,
    media_index: &mut usize,
    card: &mut Card,
) {
    let mut links = Vec::new();
    if let Some(color) = root["themeColor"].as_str().and_then(parse_hex_color) {
        card.color = Some(color);
    }
    if let Some(title) = root["title"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        card.title = clean_legacy_text(title, &mut links);
    } else if let Some(summary) = root["summary"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        card.title = clean_legacy_text(summary, &mut links);
    }
    if let Some(text) = root["text"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let cleaned = clean_legacy_text(text, &mut links);
        push_line(&mut card.text, &cleaned);
    }
    for section in root["sections"].as_array().into_iter().flatten() {
        if let Some(act_title) = section["activityTitle"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            let cleaned = clean_legacy_text(act_title, &mut links);
            if card.author.is_empty() {
                card.author = cleaned;
            } else {
                push_line(&mut card.text, &cleaned);
            }
        }
        if let Some(act_sub) = section["activitySubtitle"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            let cleaned = clean_legacy_text(act_sub, &mut links);
            push_line(&mut card.text, &cleaned);
        }
        if let Some(act_text) = section["activityText"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            let cleaned = clean_legacy_text(act_text, &mut links);
            push_line(&mut card.text, &cleaned);
        }
        if let Some(sec_title) = section["title"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            let cleaned = clean_legacy_text(sec_title, &mut links);
            if card.title.is_empty() {
                card.title = cleaned;
            } else {
                push_line(&mut card.text, &cleaned);
            }
        }
        if let Some(sec_text) = section["text"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            let cleaned = clean_legacy_text(sec_text, &mut links);
            push_line(&mut card.text, &cleaned);
        }
        for fact in section["facts"].as_array().into_iter().flatten() {
            let name = fact["name"].as_str().unwrap_or("");
            let value = fact["value"].as_str().unwrap_or("");
            let cleaned = clean_legacy_text(value, &mut links);
            push_fact(card, name, &cleaned);
        }
        if let Some(img_url) = section["activityImage"].as_str().filter(|s| !s.trim().is_empty()) {
            add_image(card, img_url, "Activity image", true, message_id, hosted, media, media_index);
        }
        for img in section["images"].as_array().into_iter().flatten() {
            let url = img["image"].as_str().unwrap_or("");
            let alt = img["title"].as_str().unwrap_or("");
            add_image(card, url, alt, false, message_id, hosted, media, media_index);
        }
        if let Some(actions) = section["potentialAction"].as_array() {
            parse_o365_actions(actions, message_link, card);
        }
    }
    if let Some(actions) = root["potentialAction"].as_array() {
        parse_o365_actions(actions, message_link, card);
    }
    for (label, url) in links {
        push_action(card, &label, &url);
    }
}

fn parse_o365_actions(actions: &[Value], message_link: &str, card: &mut Card) {
    for action in actions {
        let kind = action["@type"].as_str().unwrap_or("");
        let name = action["name"].as_str().unwrap_or("");
        if kind.eq_ignore_ascii_case("OpenUri") {
            let uri = action["targets"]
                .as_array()
                .and_then(|targets| {
                    targets
                        .iter()
                        .find(|t| t["os"].as_str().is_none_or(|os| os.eq_ignore_ascii_case("default")))
                        .or_else(|| targets.first())
                })
                .and_then(|t| t["uri"].as_str());
            if let Some(uri) = uri {
                push_action(card, name, uri);
            }
        } else if !kind.is_empty() {
            push_open_in_teams(card, name, message_link);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MSG_LINK: &str = "https://teams.microsoft.com/l/message/19%3Agen%40thread.tacv2/1760000300300?tenantId=t1";

    fn hosted_teams(url: &str) -> bool {
        url.starts_with("https://eu-api.asm.skype.com/")
    }

    #[test]
    fn adaptive_card_with_facts_columns_table_images_and_actions() {
        let payload = json!([{
            "contentType": ADAPTIVE,
            "content": json!({
                "type": "AdaptiveCard",
                "version": "1.5",
                "body": [
                    {"type": "TextBlock", "text": "**Deploy #512** failed", "weight": "bolder", "size": "large", "color": "attention"},
                    {"type": "TextBlock", "text": "Check [build logs](https://ci.example/512/logs) and `stdout`."},
                    {"type": "FactSet", "facts": [
                        {"title": "Service", "value": "payments-api"},
                        {"title": "Region", "value": "eu-west-1"}
                    ]},
                    {"type": "ColumnSet", "columns": [
                        {"type": "Column", "items": [{"type": "TextBlock", "text": "Commit"}]},
                        {"type": "Column", "items": [{"type": "TextBlock", "text": "a7fa2e22"}]}
                    ]},
                    {"type": "Table", "rows": [
                        {"type": "TableRow", "cells": [
                            {"type": "TableCell", "items": [{"type": "TextBlock", "text": "Step"}]},
                            {"type": "TableCell", "items": [{"type": "TextBlock", "text": "Status"}]}
                        ]},
                        {"type": "TableRow", "cells": [
                            {"type": "TableCell", "items": [{"type": "TextBlock", "text": "migrate"}]},
                            {"type": "TableCell", "items": [{"type": "TextBlock", "text": "failed"}]}
                        ]}
                    ]},
                    {"type": "Image", "url": "https://eu-api.asm.skype.com/v1/objects/card-img-1/views/imgo", "altText": "Graph"},
                    {"type": "Image", "url": "https://evil.example/tracker.png", "altText": "External chart"}
                ],
                "actions": [
                    {"type": "Action.OpenUrl", "title": "View PR", "url": "https://github.com/org/repo/pull/42"},
                    {"type": "Action.Submit", "title": "Retry deploy"},
                    {"type": "Action.Execute", "title": "Acknowledge"}
                ]
            }).to_string()
        }]);
        let parsed = parse_cards(payload.as_array().unwrap(), "", "1760000300300", MSG_LINK, hosted_teams);
        assert_eq!(parsed.cards.len(), 1);
        let card = &parsed.cards[0];
        assert_eq!(card.title, "Deploy #512 failed");
        assert_eq!(card.color, Some(0xD93025));
        assert!(card.text.contains("Check build logs and stdout."), "{}", card.text);
        assert!(card.text.contains("Commit · a7fa2e22"), "{}", card.text);
        assert!(card.text.contains("Step | Status"), "{}", card.text);
        assert!(card.text.contains("migrate | failed"), "{}", card.text);
        assert_eq!(card.fields, vec![("Service".into(), "payments-api".into()), ("Region".into(), "eu-west-1".into())]);
        // Only Teams-hosted image is registered for fetch; external image becomes a link action.
        assert_eq!(parsed.media.len(), 1);
        assert_eq!(parsed.media[0].1, "https://eu-api.asm.skype.com/v1/objects/card-img-1/views/imgo");
        assert!(card.image.is_some());
        assert!(card.actions.iter().any(|a| a.label == "External chart" && a.url == "https://evil.example/tracker.png"));
        assert!(card.actions.iter().any(|a| a.label == "View PR" && a.url == "https://github.com/org/repo/pull/42"));
        assert!(card.actions.iter().any(|a| a.label == "Retry deploy · Open in Teams" && a.url == MSG_LINK));
        assert!(card.actions.iter().any(|a| a.label == "Acknowledge · Open in Teams" && a.url == MSG_LINK));
    }

    #[test]
    fn o365_connector_alert_card_maps_color_sections_facts_and_actions() {
        let payload = json!([{
            "contentType": O365_CONNECTOR,
            "content": json!({
                "@type": "MessageCard",
                "themeColor": "D93025",
                "title": "High CPU on prod-db-1",
                "sections": [{
                    "activityTitle": "Datadog Monitor",
                    "activitySubtitle": "Triggered 2m ago",
                    "facts": [
                        {"name": "Host", "value": "prod-db-1"},
                        {"name": "CPU", "value": "97%"}
                    ],
                    "images": [{"image": "https://cdn.datadoghq.com/snap.png", "title": "CPU snapshot"}]
                }],
                "potentialAction": [
                    {"@type": "OpenUri", "name": "Open Monitor", "targets": [{"os": "default", "uri": "https://app.datadoghq.com/monitors/1"}]},
                    {"@type": "ActionCard", "name": "Mute 1h"}
                ]
            }).to_string()
        }]);
        let parsed = parse_cards(payload.as_array().unwrap(), "", "1760000300301", MSG_LINK, hosted_teams);
        assert_eq!(parsed.cards.len(), 1);
        assert!(parsed.media.is_empty(), "external connector image must never be fetched");
        let card = &parsed.cards[0];
        assert_eq!(card.title, "High CPU on prod-db-1");
        assert_eq!(card.author, "Datadog Monitor");
        assert_eq!(card.color, Some(0xD93025));
        assert!(card.text.contains("Triggered 2m ago"));
        assert_eq!(card.fields, vec![("Host".into(), "prod-db-1".into()), ("CPU".into(), "97%".into())]);
        assert!(card.actions.iter().any(|a| a.label == "Open Monitor" && a.url == "https://app.datadoghq.com/monitors/1"));
        assert!(card.actions.iter().any(|a| a.label == "Mute 1h · Open in Teams" && a.url == MSG_LINK));
        assert!(card.actions.iter().any(|a| a.label == "CPU snapshot" && a.url == "https://cdn.datadoghq.com/snap.png"));
    }

    #[test]
    fn hero_card_maps_title_subtitle_text_images_and_buttons() {
        let payload = json!([{
            "contentType": HERO,
            "content": {
                "title": "Pull Request #88",
                "subtitle": "tinyclient / main",
                "text": "<b>Ready for review</b> by @platform",
                "tap": {"type": "openUrl", "value": "https://github.com/org/tinyclient/pull/88"},
                "images": [{"url": "https://avatars.githubusercontent.com/u/1", "alt": "Author avatar"}],
                "buttons": [
                    {"type": "openUrl", "title": "Open on GitHub", "value": "https://github.com/org/tinyclient/pull/88"},
                    {"type": "imBack", "title": "Approve", "value": "approve 88"}
                ]
            }
        }]);
        let parsed = parse_cards(payload.as_array().unwrap(), "", "1760000300302", MSG_LINK, hosted_teams);
        assert_eq!(parsed.cards.len(), 1);
        assert!(parsed.media.is_empty(), "external hero image must never be fetched");
        let card = &parsed.cards[0];
        assert_eq!(card.title, "Pull Request #88");
        assert_eq!(card.title_link, "https://github.com/org/tinyclient/pull/88");
        assert_eq!(card.author, "tinyclient / main");
        assert_eq!(card.text, "Ready for review by @platform");
        assert!(card.actions.iter().any(|a| a.label == "Open on GitHub" && a.url == "https://github.com/org/tinyclient/pull/88"));
        assert!(card.actions.iter().any(|a| a.label == "Approve · Open in Teams" && a.url == MSG_LINK));
        assert!(card.actions.iter().any(|a| a.label == "Author avatar" && a.url == "https://avatars.githubusercontent.com/u/1"));
    }

    #[test]
    fn malformed_or_empty_card_yields_nothing_for_clean_fallback() {
        let bad = json!([{"contentType": ADAPTIVE, "content": "{not json"}]);
        assert!(parse_cards(bad.as_array().unwrap(), "", "1", MSG_LINK, hosted_teams).cards.is_empty());
        let empty = json!([{"contentType": ADAPTIVE, "content": "{\"type\":\"AdaptiveCard\",\"body\":[]}"}]);
        assert!(parse_cards(empty.as_array().unwrap(), "", "1", MSG_LINK, hosted_teams).cards.is_empty());
    }
}
