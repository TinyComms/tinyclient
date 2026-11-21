//! One Teams message, from push or from a page of history, as the one message model; and
//! everything done to messages: pages, sending, editing, deleting, reacting.

use super::html::{self, Lookup};
use super::{chain_id, chain_of, escaped, id_of, millis, now_ms, seconds, thread_of, Body, Fault, Teams, CHAIN, PAGE, TEAMS, TYPING_SHOWN};
use protocol::{
    Attachment, ConversationKind, Forwarded, MediaFile, MediaKind, MentionKind, Message,
    MessageKind, Preview, Reaction, ReplyTo, Span, Style, ThreadSummary,
};
use serde_json::{json, Value};

/// What reactions a Teams conversation takes. The catalog in `emoji` names every
/// emoji Teams has a key for, so this is not a short list. A skin tone is sent as
/// `key-toneN` and stays. A reaction already on a message brought its own key and
/// can be added to even when the catalog has no name for it. Teams lets a person
/// give several; no limit on them or on different emoji is documented, so none is set.
pub(crate) fn reaction_rules() -> protocol::ReactionRules {
    protocol::ReactionRules { set: protocol::ReactionSet::Any, tones: true, ..protocol::ReactionRules::default() }
}

/// The lab's teams-reactions driver. Every catalog emoji maps to a key and back,
/// and a skin tone is `key-toneN`.
pub(crate) fn prove_reactions() -> Result<(), String> {
    super::emoji::prove_catalog()?;
    let rules = reaction_rules();
    if rules.set != protocol::ReactionSet::Any || !rules.tones || !rules.only.is_empty() {
        return Err("Teams reactions are still a short list, or a skin tone would be dropped".to_owned());
    }
    Ok(())
}

/// An emoji without the marks that do not change which one it is.
fn bare_emoji(emoji: &str) -> String {
    emoji.chars().filter(|c| *c != '\u{fe0f}' && !('\u{1f3fb}'..='\u{1f3ff}').contains(c)).collect()
}

/// The emoji a reaction key is. A key Teams added since reads as `:key:`.
fn emoji_of(key: &str) -> String {
    super::emoji::emoji_of(key)
}

/// The key Teams knows an emoji by, including a skin tone's `-toneN`.
fn key_of(emoji: &str) -> Option<String> {
    super::emoji::reaction_key(emoji)
}

/// The person id at the end of `https://…/v1/users/ME/contacts/8:orgid:…`, or the id itself.
fn person(from: &str) -> &str {
    from.rsplit('/').next().unwrap_or(from)
}

/// The conversation a message is in and, in a channel, the post its reply chain hangs off:
/// `…/conversations/19:x@thread.tacv2;messageid=1700000000000`. Newer threads-layout channels may
/// carry `rootMessageId` or `properties.parentMessageId` instead of `;messageid=`.
fn placed(raw: &Value) -> Option<(String, Option<String>)> {
    let link = raw["conversationLink"].as_str().unwrap_or("");
    let named = link.split_once("/conversations/").map(|(_, rest)| rest.split('/').next().unwrap_or(rest));
    let named = named.or(raw["conversationid"].as_str()).filter(|named| !named.is_empty())?;
    let (channel, mut root) = match chain_of(named) {
        Some((channel, root)) => (channel.to_owned(), Some(root.to_owned())),
        None => (named.to_owned(), None),
    };
    if root.is_none() {
        let fallback = [&raw["rootMessageId"], &raw["parentMessageId"], &raw["properties"]["parentMessageId"]]
            .into_iter()
            .map(id_of)
            .find(|id| millis(id) > 0);
        if let Some(parent) = fallback {
            root = Some(parent);
        }
    }
    Some((channel, root))
}

/// Detects a Teams channel announcement post and extracts its headline and optional subhead.
/// Any banner image or color is treated as decoration only (never fetched).
fn announcement_parts(props: &Value, content: &str) -> Option<(String, Option<String>)> {
    let parsed_ann = match &props["announcement"] {
        Value::String(text) if text.trim().starts_with('{') => serde_json::from_str::<Value>(text).ok(),
        Value::Object(_) => Some(props["announcement"].clone()),
        _ => None,
    };
    let is_ann = props["cardType"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("announcement"))
        || props["postType"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("announcement"))
        || props["isAnnouncement"] == true
        || props["isAnnouncement"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("true"))
        || parsed_ann.is_some()
        || props["bannerImage"].as_str().is_some_and(|s| !s.trim().is_empty())
        || props["bannerColor"].as_str().is_some_and(|s| !s.trim().is_empty())
        || props["headerImage"].as_str().is_some_and(|s| !s.trim().is_empty())
        || content.contains("http://schema.skype.com/Announcement");
    if !is_ann {
        return None;
    }
    let ann_title = parsed_ann.as_ref().and_then(|v| v["title"].as_str()).map(str::trim).filter(|s| !s.is_empty());
    let ann_sub = parsed_ann.as_ref().and_then(|v| v["subhead"].as_str().or(v["subtitle"].as_str())).map(str::trim).filter(|s| !s.is_empty());
    let title = ann_title
        .or_else(|| props["title"].as_str().map(str::trim).filter(|s| !s.is_empty()))
        .or_else(|| props["subject"].as_str().map(str::trim).filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_owned();
    let subhead = ann_sub
        .or_else(|| props["subhead"].as_str().map(str::trim).filter(|s| !s.is_empty()))
        .or_else(|| props["subject"].as_str().map(str::trim).filter(|s| !s.is_empty() && *s != title))
        .map(str::to_owned);
    Some((title, subhead))
}

/// A value Teams writes as a string, a number, or leaves out: `edittime`, `deletetime`.
fn set(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.is_empty() && text != "0" && text != "null",
        Value::Number(number) => number.as_f64().is_some_and(|number| number > 0.),
        _ => false,
    }
}

/// Maps Teams' `properties.importance` (`high` / `urgent`) into [`protocol::Importance`].
fn importance_of(value: &Value) -> protocol::Importance {
    match value.as_str().map(str::trim).unwrap_or("").to_ascii_lowercase().as_str() {
        "urgent" => protocol::Importance::Urgent,
        "high" | "important" => protocol::Importance::Important,
        _ => protocol::Importance::Normal,
    }
}

/// Wire value for Teams' `properties.importance` when sending.
fn importance_wire(importance: protocol::Importance) -> Option<&'static str> {
    match importance {
        protocol::Importance::Normal => None,
        protocol::Importance::Important => Some("high"),
        protocol::Importance::Urgent => Some("urgent"),
    }
}

/// A property that holds JSON as text, or the JSON itself.
fn listed(value: &Value) -> Vec<Value> {
    match value {
        Value::String(text) => serde_json::from_str::<Vec<Value>>(text).unwrap_or_default(),
        Value::Array(items) => items.clone(),
        _ => Vec::new(),
    }
}

/// The text between `<name>` and `</name>`, each time it appears.
fn inside<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some((_, after)) = rest.split_once(open.as_str()) {
        let Some((text, tail)) = after.split_once(close.as_str()) else {
            break;
        };
        found.push(text);
        rest = tail;
    }
    found
}

/// Microsoft Graph names a Loop component `application/vnd.microsoft.card.fluidEmbedCard`.
/// Its file address is `componentUrl` inside `content`. The chat service keeps that card
/// in `properties.cards`, the same place as an adaptive card.
const FLUID_CARD: &str = "application/vnd.microsoft.card.fluidEmbedCard";

/// A Loop component read out of a card list: the row's title, and the file to open.
struct LoopCard {
    title: String,
    url: String,
}

/// The first Loop component in a card list. An adaptive card is not one.
fn loop_card(cards: &[Value]) -> Option<LoopCard> {
    cards.iter().find_map(|card| {
        let kind = card.get("contentType").and_then(Value::as_str).unwrap_or("");
        let app = card.get("teamsAppId").and_then(Value::as_str).unwrap_or("");
        if kind != FLUID_CARD && app != "FluidEmbedCard" {
            return None;
        }
        let content = match card.get("content") {
            Some(Value::String(text)) => serde_json::from_str::<Value>(text).unwrap_or(Value::Null),
            Some(other) => other.clone(),
            None => Value::Null,
        };
        let url = content
            .get("componentUrl")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| url.starts_with("https://"))
            .unwrap_or("")
            .to_owned();
        let title = card.get("name").and_then(Value::as_str).map(str::trim).filter(|name| !name.is_empty()).unwrap_or("").to_owned();
        Some(LoopCard { title, url })
    })
}

/// `Loop component` or `Loop component · <title>` when the card names itself.
fn loop_label(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        "Loop component".to_owned()
    } else {
        format!("Loop component · {}", title.chars().take(200).collect::<String>())
    }
}

/// The lab's teams-loop-component driver. A fluid embed card is labelled and linked to its
/// file. An adaptive card stays an ordinary card. A file address that is not https is dropped.
pub(crate) fn prove_loop_component() -> Result<(), String> {
    let url = "https://contoso.sharepoint.com/:fl:/g/personal/ana/EaBcDeF";
    let named = json!([{
        "contentType": FLUID_CARD,
        "teamsAppId": "FluidEmbedCard",
        "name": "Sprint checklist",
        "content": json!({"componentUrl": url, "sourceType": "Compose"}).to_string(),
    }]);
    let found = loop_card(named.as_array().unwrap()).ok_or_else(|| "a Loop component is still read as an ordinary card".to_owned())?;
    if found.url != url {
        return Err(format!("a Loop component does not link to its file: {}", found.url));
    }
    if loop_label(&found.title) != "Loop component · Sprint checklist" {
        return Err(format!("a Loop component reads as {}", loop_label(&found.title)));
    }
    let bare = json!([{
        "contentType": FLUID_CARD,
        "content": json!({"componentUrl": "https://contoso.sharepoint.com/personal/ana/standup.loop", "sourceType": "Compose"}).to_string(),
    }]);
    let bare = loop_card(bare.as_array().unwrap()).ok_or_else(|| "a Loop component without a name was dropped".to_owned())?;
    if loop_label(&bare.title) != "Loop component" || !bare.url.ends_with(".loop") {
        return Err("a Loop component without a name was not labelled".to_owned());
    }
    let adaptive = json!([{
        "contentType": "application/vnd.microsoft.card.adaptive",
        "content": "{\"type\":\"AdaptiveCard\",\"body\":[{\"type\":\"TextBlock\",\"text\":\"Build\"}]}",
    }]);
    if loop_card(adaptive.as_array().unwrap()).is_some() {
        return Err("an adaptive card was labelled as a Loop component".to_owned());
    }
    let plain = json!([{
        "contentType": FLUID_CARD,
        "content": "{\"componentUrl\": \"http://contoso.sharepoint.com/x.loop\", \"sourceType\": \"Compose\"}",
    }]);
    let plain = loop_card(plain.as_array().unwrap()).ok_or_else(|| "a Loop component with a non-https file was dropped".to_owned())?;
    if !plain.url.is_empty() {
        return Err("a Loop component kept a file address that is not https".to_owned());
    }
    Ok(())
}

/// The first title or text a card carries, looked for a few levels down.
fn card_title(card: &Value, depth: u8) -> Option<String> {
    if depth == 0 {
        return None;
    }
    match card {
        Value::Object(fields) => {
            let own = ["title", "text", "subtitle"].iter().filter_map(|name| fields.get(*name)?.as_str()).find(|text| !text.trim().is_empty());
            // What a card says comes before what its buttons say.
            let inner = ["content", "body"].iter().filter_map(|name| fields.get(*name)).chain(fields.values());
            own.map(str::to_owned).or_else(|| inner.into_iter().find_map(|inner| card_title(inner, depth - 1)))
        }
        Value::Array(items) => items.iter().find_map(|inner| card_title(inner, depth - 1)),
        Value::String(text) if text.starts_with('{') => card_title(&serde_json::from_str(text).ok()?, depth - 1),
        _ => None,
    }
}

/// The shell's `client_id` on a copy of a message we sent. Teams names it `clientmessageid`,
/// as text or as a number, and the store replaces the pending row only when it comes back on
/// the message. Someone else's copy, or an id that is already the message's own, is not one.
fn echoed_client_id(raw: &Value, outgoing: bool, id: &str) -> Option<String> {
    if !outgoing {
        return None;
    }
    let echoed = id_of(&raw["clientmessageid"]);
    (!echoed.is_empty() && echoed != id).then_some(echoed)
}

/// `Event/Call`: `<partlist type="ended" …><part …><duration>125</duration></part></partlist>`.
fn call_line(content: &str) -> String {
    let longest = inside(content, "duration").iter().filter_map(|text| text.trim().parse::<f64>().ok()).fold(0., f64::max);
    if content.contains("type=\"missed\"") {
        "Missed call".to_owned()
    } else if content.contains("type=\"ended\"") || content.contains("<ended") {
        match (longest / 60.).round() as u64 {
            0 => "A call ended".to_owned(),
            minutes => format!("A call ended · {minutes} min"),
        }
    } else {
        "A call started".to_owned()
    }
}

/// How a shared file is shown. A picture or a video is the row; anything else is a document.
pub(super) fn shared_kind(name: &str) -> MediaKind {
    let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => MediaKind::Image,
        "mp4" | "mov" | "webm" => MediaKind::Video,
        _ => MediaKind::File,
    }
}

/// The size Teams draws a Giphy GIF at, about: what the small copy is asked for.
const GIPHY_TALL: u32 = 200;

/// A GIF from Teams' Giphy button as an attachment: the original GIF for opening, and for the
/// row Giphy's own 200 px tall WebP of it (`200.webp`, its "fixed height" copy), so the row
/// plays for a fraction of the original. An address not in Giphy's usual shape has no smaller
/// copy and is shown as it is.
fn giphy_gif(gif: &html::Image, media: &mut Vec<(MediaFile, String)>) -> Attachment {
    let path = gif.src.split(['?', '#']).next().unwrap_or(&gif.src);
    let object = path.trim_end_matches("/giphy.gif").rsplit('/').next().unwrap_or(path);
    let file = MediaFile {
        width: gif.width,
        height: gif.height,
        ..MediaFile::new(format!("giphy:{}", escaped(object)), MediaKind::Gif)
    };
    media.push((file.clone(), gif.src.clone()));
    let thumb = path.strip_suffix("/giphy.gif").filter(|_| path.contains("/media/")).map(|base| {
        let (width, height) = match (gif.width, gif.height) {
            (w, h) if w > 0 && h > 0 => ((u64::from(w) * u64::from(GIPHY_TALL) / u64::from(h)).max(1) as u32, GIPHY_TALL),
            _ => (0, 0),
        };
        let thumb = MediaFile { width, height, ..MediaFile::new(format!("{}:small", file.id), MediaKind::Image) };
        media.push((thumb.clone(), format!("{base}/{GIPHY_TALL}.webp")));
        thumb
    });
    let named = if gif.name.trim().is_empty() { "GIF" } else { gif.name.trim() };
    Attachment {
        thumb: Some(thumb.unwrap_or_else(|| file.clone())),
        mime: "image/gif".to_owned(),
        ..Attachment::new(named, file)
    }
}

/// Metadata remembered for our own messages so editing them preserves subject and inline images.
#[derive(Debug, Clone, Default)]
pub(super) struct OwnMeta {
    pub subject: Option<String>,
    pub images_html: String,
}

/// What one of Teams' message objects turned out to be.
enum Read {
    Message(Box<Message>),
    Deleted(String),
    Typing { person: String, name: String, on: bool },
    /// Housekeeping no reader needs to see.
    Nothing,
}

struct Refs<'a> {
    teams: &'a Teams,
    mentions: &'a [Value],
}

impl Lookup for Refs<'_> {
    fn mention(&self, item: &str) -> Option<(MentionKind, String)> {
        let named = self.mentions.iter().find(|mention| id_of(&mention["itemid"]) == item)?;
        let id = named["mri"].as_str().unwrap_or("").to_owned();
        Some(match named["mentionType"].as_str().unwrap_or("person") {
            "person" | "bot" => (MentionKind::User, id),
            "tag" => (MentionKind::Role, id),
            // A channel, a team or everyone in a chat: all of them call on whoever is in it.
            _ => (MentionKind::Everyone, id),
        })
    }

    fn hosted(&self, url: &str) -> bool {
        self.teams.pictures(url)
    }

    fn emoji(&self, id: &str, url: &str) -> Option<MediaFile> {
        if id.is_empty() || !self.teams.pictures(url) {
            return None;
        }
        let file = MediaFile::new(format!("emoji:{id}"), MediaKind::Emoji);
        self.teams.ctx.media(&[(file.clone(), url.to_owned())]).ok()?;
        Some(file)
    }
}

/// Extracts the first `https://` or `http://` URL from outgoing text.
pub(super) fn first_url(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| matches!(c, '<' | '>' | '(' | ')' | '[' | ']' | '"' | '\''));
        let cleaned = trimmed.trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?'));
        if (cleaned.starts_with("https://") || cleaned.starts_with("http://")) && cleaned.len() > 10 {
            return Some(cleaned.to_owned());
        }
    }
    None
}

/// Maps a Microsoft 365 file extension to a human-readable app label.
fn m365_app(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "doc" | "docx" => Some("Word"),
        "xls" | "xlsx" | "csv" => Some("Excel"),
        "ppt" | "pptx" => Some("PowerPoint"),
        "one" => Some("OneNote"),
        "pdf" => Some("PDF"),
        _ => None,
    }
}

/// Parses Teams `properties.links` and `properties.urlpreviews` into `protocol::Preview` cards,
/// including Microsoft 365 file link metadata. Preview images are registered only when `hosted`
/// confirms they live on Teams' own media host.
pub(super) fn parse_link_previews(
    props: &Value,
    msg_id: &str,
    hosted: impl Fn(&str) -> bool,
    media: &mut Vec<(MediaFile, String)>,
) -> Vec<Preview> {
    let mut entries = listed(&props["links"]);
    entries.extend(listed(&props["urlpreviews"]));
    let mut previews = Vec::new();
    for (idx, item) in entries.into_iter().enumerate() {
        let url = [&item["url"], &item["originalUrl"], &item["href"], &item["webUrl"]]
            .into_iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|u| u.starts_with("https://") || u.starts_with("http://"))
            .unwrap_or("");
        if url.is_empty() || previews.iter().any(|p: &Preview| p.url == url) {
            continue;
        }
        let file_name = [&item["fileName"], &item["name"]]
            .into_iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|s| !s.is_empty());
        let file_type = [&item["fileType"], &item["type"]]
            .into_iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|s| !s.is_empty())
            .unwrap_or("");
        let host = super::https_host(url).unwrap_or("");
        let is_m365 = file_name.is_some()
            || super::within(host, "sharepoint.com")
            || super::within(host, "1drv.ms")
            || super::within(host, "onedrive.live.com")
            || super::within(host, "office.com")
            || item["@type"].as_str().is_some_and(|t| t.contains("File") || t.contains("SharePoint"));
        let title = [&item["title"], &item["previewTitle"], &item["fileName"], &item["name"]]
            .into_iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_owned();
        let description = [&item["description"], &item["previewDescription"], &item["snippet"]]
            .into_iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_owned();
        let raw_site = [&item["site"], &item["siteName"], &item["domain"]]
            .into_iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|s| !s.is_empty());
        let site = if is_m365 {
            let service = raw_site.unwrap_or_else(|| {
                if super::within(host, "sharepoint.com") {
                    "SharePoint"
                } else if super::within(host, "1drv.ms") || super::within(host, "onedrive.live.com") {
                    "OneDrive"
                } else {
                    "Microsoft 365"
                }
            });
            let ext = if !file_type.is_empty() && !file_type.contains('/') {
                file_type
            } else {
                file_name.and_then(|n| n.rsplit_once('.').map(|(_, e)| e)).unwrap_or("")
            };
            match m365_app(ext) {
                Some(app) if !service.eq_ignore_ascii_case(app) => format!("{app} · {service}"),
                _ => service.to_owned(),
            }
        } else {
            raw_site.map(str::to_owned).unwrap_or_else(|| host.to_owned())
        };
        let img_url = [
            item["image"].as_str(),
            item["previewImage"].as_str(),
            item["previewImage"]["url"].as_str(),
            item["thumbnailUrl"].as_str(),
            item["imageUrl"].as_str(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|u| !u.is_empty() && hosted(u));
        let image = img_url.map(|u| {
            let file = MediaFile::new(format!("preview:{msg_id}:{idx}"), MediaKind::Image);
            media.push((file.clone(), u.to_owned()));
            file
        });
        if title.is_empty() && description.is_empty() && image.is_none() && !is_m365 {
            continue;
        }
        previews.push(Preview {
            url: url.to_owned(),
            site,
            title,
            description,
            image,
        });
    }
    previews
}

/// Counts non-outgoing messages newer than the consumption horizon (`last_read`) and how many
/// mention us. When no horizon is known, counts non-outgoing messages newer than our latest own
/// message on the page.
pub(super) fn count_unread_against_horizon(messages: &[Message], last_read: Option<&str>) -> (u32, u32) {
    let horizon = last_read.map(millis).filter(|ms| *ms > 0).unwrap_or_else(|| {
        messages
            .iter()
            .filter(|m| m.outgoing)
            .map(|m| millis(&m.id))
            .max()
            .unwrap_or(0)
    });
    let mut unread = 0u32;
    let mut mentions = 0u32;
    for msg in messages {
        if msg.outgoing || msg.deleted || msg.kind == MessageKind::System {
            continue;
        }
        let ts = millis(&msg.id);
        if ts > 0 && ts > horizon {
            unread = unread.saturating_add(1);
            if msg.mentions_me {
                mentions = mentions.saturating_add(1);
            }
        }
    }
    (unread, mentions)
}

/// The header of a quoted reply, as Teams' own client writes it.
fn quote_markup(reply: &ReplyTo) -> String {
    // A quote's name and preview are already plain text. Escaping them must not read markdown.
    let text = |text: &str| html::escape_plain(text);
    let person = reply.person.as_ref().map_or("", |person| person.id.as_str());
    format!(
        "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Reply\" itemid=\"{id}\">\
         <strong itemprop=\"mri\" itemid=\"{person}\">{name}</strong><span itemprop=\"time\" itemid=\"{id}\"></span>\
         <p itemprop=\"preview\">{preview}</p></blockquote>",
        id = text(&reply.id),
        person = text(person),
        name = text(&reply.sender),
        preview = text(&reply.snippet),
    )
}

/// The forwarded message block, as Teams' client writes it, carrying original sender, time,
/// source conversation, link back to the original, and body.
fn forward_markup(original: &Message, from_title: &str, link: &str) -> String {
    let text = |text: &str| html::escape_plain(text);
    let sent_ms = match millis(&original.id) {
        0 => original.sent.saturating_mul(1000).to_string(),
        ms => ms.to_string(),
    };
    let mut out = format!(
        "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Forward\" itemid=\"{id}\">\
         <strong itemprop=\"mri\" itemid=\"{person}\">{name}</strong>\
         <span itemprop=\"time\" itemid=\"{sent_ms}\"></span>",
        id = text(&original.id),
        person = text(&original.sender.id),
        name = text(original.shown_sender()),
    );
    if !from_title.is_empty() {
        out.push_str(&format!("<span itemprop=\"conversation\">{}</span>", text(from_title)));
    }
    if !link.is_empty() {
        out.push_str(&format!("<a itemprop=\"sourceLink\" href=\"{}\"></a>", text(link)));
    }
    out.push_str(&format!("<p itemprop=\"preview\">{}</p></blockquote>", text(&original.text)));
    out
}

/// Parses `properties.forwardedMessage` / `properties.forwardContext` when present as JSON.
fn forwarded_property(props: &Value) -> Option<Value> {
    for key in ["forwardedMessage", "forwardContext", "forwarded"] {
        match props.get(key) {
            Some(Value::Object(_)) => return Some(props[key].clone()),
            Some(Value::String(s)) if s.trim().starts_with('{') => {
                if let Ok(v) = serde_json::from_str::<Value>(s) {
                    if v.is_object() {
                        return Some(v);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

impl Teams {
    /// Where the official app opens one message. A reply names its post as `parentMessageId`.
    fn message_link(&self, conversation: &str, id: &str, raw: &Value) -> String {
        let placed = placed(raw);
        let thread = placed.as_ref().map(|(channel, _)| channel.as_str()).unwrap_or_else(|| thread_of(conversation));
        let parent = placed.as_ref().and_then(|(_, root)| root.as_deref()).filter(|root| *root != id);
        let parent = parent.or_else(|| chain_of(conversation).map(|(_, root)| root).filter(|root| *root != id));
        super::links::message_share(thread, id, &self.session.tenant, parent)
    }

    /// `ThreadActivity/…`: who joined, left or renamed the conversation, in words. Names come
    /// from what is already known; nobody is looked up for a grey line.
    fn activity(&self, kind: &str, content: &str) -> Option<String> {
        let name = |id: &str| self.name_of(id.trim()).unwrap_or_else(|| "Someone".to_owned());
        let initiator = inside(content, "initiator").first().map(|id| name(id));
        let targets: Vec<&str> = inside(content, "target").into_iter().map(str::trim).collect();
        let own = inside(content, "initiator").first().is_some_and(|id| targets.len() == 1 && id.trim() == targets[0]);
        let people = |ids: &[&str]| match ids {
            [] => "someone".to_owned(),
            [one] => name(one),
            [first, rest @ ..] => format!("{} and {} more", name(first), rest.len()),
        };
        let joined = |verb: &str| {
            let members: Value = serde_json::from_str(content).ok()?;
            let names: Vec<&str> = members["members"].as_array()?.iter().filter_map(|member| member["friendlyname"].as_str()).collect();
            let who = match names[..] {
                [] => "Someone".to_owned(),
                [one] => one.to_owned(),
                [first, ..] => format!("{first} and {} more", names.len() - 1),
            };
            Some(format!("{who} {verb}"))
        };
        match kind.strip_prefix("ThreadActivity/")? {
            "AddMember" if own => Some(format!("{} joined", people(&targets))),
            "AddMember" => Some(format!("{} added {}", initiator.unwrap_or_else(|| "Someone".to_owned()), people(&targets))),
            "DeleteMember" if own => Some(format!("{} left", people(&targets))),
            "DeleteMember" => Some(format!("{} removed {}", initiator.unwrap_or_else(|| "Someone".to_owned()), people(&targets))),
            "TopicUpdate" => {
                let topic = inside(content, "value").first().map(|topic| html::parse(topic, &Refs { teams: self, mentions: &[] }, "").text)?;
                Some(format!("{} named the conversation {topic}", initiator.unwrap_or_else(|| "Someone".to_owned())))
            }
            "MemberJoined" => joined("joined"),
            "MemberLeft" => joined("left"),
            "PictureUpdate" => Some("The picture was changed".to_owned()),
            _ => None,
        }
    }

    /// Parses a raw Teams message JSON into a `Message`, if it is a displayable message.
    pub(super) fn read_message(&self, conversation: &str, raw: &Value) -> Option<Message> {
        match self.read(conversation, raw) {
            Read::Message(msg) => Some(*msg),
            _ => None,
        }
    }

    /// Reads one message object. `conversation` is where it will be recorded.
    fn read(&self, conversation: &str, raw: &Value) -> Read {
        let id = id_of(&raw["id"]);
        let kind = raw["messagetype"].as_str().unwrap_or("");
        let from = person(raw["from"].as_str().unwrap_or("")).to_owned();
        let props = &raw["properties"];
        let me = self.me();
        if let Some(control) = kind.strip_prefix("Control/") {
            return match control {
                "Typing" | "ClearTyping" => Read::Typing {
                    name: raw["imdisplayname"].as_str().unwrap_or("").to_owned(),
                    on: control == "Typing",
                    person: from,
                },
                _ => Read::Nothing,
            };
        }
        if id.is_empty() {
            return Read::Nothing;
        }
        if set(&props["deletetime"]) || set(&props["deletionTime"]) || set(&raw["deletetime"]) {
            return Read::Deleted(id);
        }
        let name = match raw["imdisplayname"].as_str().filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => self.name_of(&from).unwrap_or_default(),
        };
        let sent = match millis(&id) / 1000 {
            0 => seconds(raw["composetime"].as_str().or(raw["originalarrivaltime"].as_str()).unwrap_or("")),
            sent => sent,
        };
        let scheduled_at = props["scheduledTime"].as_str().map(seconds).filter(|at| *at > 0);
        let is_scheduled = scheduled_at.is_some_and(|at| kind == "Scheduled" || at > sent);
        let msg_id = if is_scheduled { format!("scheduled:{id}") } else { id.clone() };
        let msg_sent = if is_scheduled { scheduled_at.unwrap_or(sent) } else { sent };
        let mut message = Message::plain(msg_id, name.clone(), "", msg_sent, from == me);
        message.sender.bot = from.starts_with("28:");
        message.sender.avatar = self.picture(&from, &name);
        message.sender.id = from;
        message.edited = set(&props["edittime"]);
        message.edited_at = props["edittime"].as_str().filter(|_| message.edited).map(seconds).filter(|at| *at > 0);
        message.importance = importance_of(&props["importance"]);
        let pin_entry = {
            let state = self.state();
            let thread = thread_of(conversation);
            let pinned_match = state
                .pinned_messages
                .get(thread)
                .and_then(|list| list.iter().find(|e| e.message_id == id))
                .cloned();
            let saved = state.saved_ids.contains(&id);
            (pinned_match, saved)
        };
        if props["pinned"] == true
            || props["pinned"].as_str().is_some_and(|v| v.eq_ignore_ascii_case("true"))
            || pin_entry.0.is_some()
        {
            message.pinned = true;
            if let Some(entry) = pin_entry.0 {
                message.pinned_by = entry.pinned_by.and_then(|who| {
                    if who.starts_with("8:") || who.starts_with("28:") {
                        self.name_of(&who).or(Some(who))
                    } else {
                        Some(who)
                    }
                });
                message.pinned_at = entry.pinned_at;
            }
        }
        if props["saved"] == true
            || props["saved"].as_str().is_some_and(|v| v.eq_ignore_ascii_case("true"))
            || pin_entry.1
        {
            message.starred = true;
        }
        let content = raw["content"].as_str().unwrap_or("");
        match kind {
            "Event/Call" => {
                message.kind = MessageKind::System;
                message.text = call_line(content);
                return Read::Message(Box::new(message));
            }
            kind if kind.starts_with("ThreadActivity/") => {
                let Some(text) = self.activity(kind, content) else {
                    return Read::Nothing;
                };
                message.kind = MessageKind::System;
                message.text = text;
                return Read::Message(Box::new(message));
            }
            "RichText/Media_CallRecording" | "RichText/Media_CallTranscript" => {
                message.kind = MessageKind::Unsupported { link: self.message_link(conversation, &id, raw) };
                message.text = "Meeting recording".to_owned();
                return Read::Message(Box::new(message));
            }
            _ => {}
        }
        let mentions = listed(&props["mentions"]);
        let refs = Refs { teams: self, mentions: &mentions };
        let mut parsed = match kind {
            // Plain text is taken as written, not read for markup.
            "Text" => html::parse(&format!("<p>{}</p>", html::escape_plain(content)), &refs, &me),
            _ => html::parse(content, &refs, &me),
        };
        // A channel post's headline or announcement header is part of what it says.
        let announcement = announcement_parts(props, content);
        let subject = if let Some((ref headline, ref subhead)) = announcement {
            message.announcement = true;
            if !headline.is_empty() {
                Some(headline.as_str())
            } else {
                subhead.as_deref()
            }
        } else {
            [&props["subject"], &props["title"]]
                .into_iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .find(|subject| !subject.is_empty())
        };
        let announcement_subhead = announcement.as_ref().and_then(|(h, s)| s.as_deref().filter(|sub| !h.is_empty() && *sub != h));
        if message.outgoing {
            let subject_owned = subject.map(str::to_owned);
            let images_html = html::inline_images_html(content, |url| self.pictures(url));
            if subject_owned.is_some() || !images_html.is_empty() {
                let mut state = self.state();
                if state.own_meta.len() >= super::REMEMBERED {
                    state.own_meta.clear();
                }
                state.own_meta.insert((conversation.to_owned(), id.clone()), OwnMeta { subject: subject_owned, images_html });
            }
        }
        if let Some(subhead) = announcement_subhead {
            let shift = (subhead.len() + usize::from(!parsed.text.is_empty())) as u32;
            for span in &mut parsed.spans {
                span.start += shift;
                span.end += shift;
            }
            parsed.text = if parsed.text.is_empty() { subhead.to_owned() } else { format!("{subhead}\n{}", parsed.text) };
            parsed.spans.insert(0, Span { start: 0, end: subhead.len() as u32, style: Style::Italic });
        }
        if let Some(subject) = subject.map(str::trim).filter(|s| !s.is_empty()) {
            let shift = (subject.len() + usize::from(!parsed.text.is_empty())) as u32;
            for span in &mut parsed.spans {
                span.start += shift;
                span.end += shift;
            }
            parsed.text = if parsed.text.is_empty() { subject.to_owned() } else { format!("{subject}\n{}", parsed.text) };
            parsed.spans.insert(0, Span { start: 0, end: subject.len() as u32, style: Style::Bold });
        }
        let mut media = Vec::new();
        for image in parsed.images {
            // `…/v1/objects/{id}/views/imgo`: the object is the picture, whatever view is asked.
            let object = image.src.split("/objects/").nth(1).and_then(|rest| rest.split('/').next()).unwrap_or(&image.src);
            let file = MediaFile {
                width: image.width,
                height: image.height,
                ..MediaFile::new(format!("img:{}", escaped(object)), MediaKind::Image)
            };
            media.push((file.clone(), image.src.clone()));
            let named = if image.name.trim().is_empty() { "image" } else { image.name.trim() };
            message.attachments.push(Attachment {
                thumb: Some(file.clone()),
                ..Attachment::new(named, file)
            });
        }
        for gif in parsed.gifs {
            message.attachments.push(giphy_gif(&gif, &mut media));
        }
        let shared_files = listed(&props["files"]);
        if !shared_files.is_empty() {
            let mut state = self.state();
            if state.file_props.len() >= super::REMEMBERED {
                state.file_props.clear();
            }
            state.file_props.insert((conversation.to_owned(), id.clone()), shared_files.clone());
        }
        for shared in shared_files {
            let info = &shared["fileInfo"];
            let urls = [&info["shareUrl"], &shared["objectUrl"], &info["fileUrl"]];
            let Some(url) = urls.into_iter().filter_map(Value::as_str).find(|url| url.starts_with("https://")) else {
                continue;
            };
            let named = [&shared["fileName"], &shared["title"]].into_iter().filter_map(Value::as_str).find(|name| !name.is_empty()).unwrap_or("File");
            let sizes = [&shared["fileSize"], &shared["size"], &info["fileSize"]];
            let key = Some(id_of(&shared["id"])).filter(|key| !key.is_empty()).unwrap_or_else(|| id_of(&shared["itemid"]));
            let kind = shared_kind(named);
            let file = MediaFile {
                bytes: sizes.into_iter().find_map(Value::as_u64).unwrap_or(0),
                ..MediaFile::new(format!("file:{id}:{}", escaped(&key)), kind)
            };
            // The cache fetches Graph's content address. The sharing address stays the link.
            media.push((file.clone(), super::graph_content(&self.graph, url)));
            let shown = matches!(kind, MediaKind::Image | MediaKind::Video);
            message.attachments.push(Attachment {
                thumb: (kind == MediaKind::Image).then(|| file.clone()),
                ..Attachment::new(named, file)
            });
            // A picture or a video is the row. A document keeps its name as a link, so it can
            // still open in the browser.
            if !shown {
                if !parsed.text.is_empty() {
                    parsed.text.push('\n');
                }
                let start = parsed.text.len() as u32;
                parsed.text.push_str(named);
                parsed.spans.push(Span { start, end: parsed.text.len() as u32, style: Style::Link { url: url.to_owned() } });
            }
        }
        message.previews = parse_link_previews(props, &id, |url| self.pictures(url) || self.ours(url), &mut media);
        if let Err(err) = self.ctx.media(&media) {
            tracing::warn!("chatd: Teams: {err}");
        }
        message.text = parsed.text;
        message.spans = parsed.spans;
        message.mentions_me = parsed.mentioned && !message.outgoing;
        message.reply_to = parsed.quote.map(|quote| ReplyTo {
            person: Some(quote.mri).filter(|mri| !mri.is_empty()).map(|mri| self.ctx.key(&mri)),
            id: quote.id,
            sender: quote.name,
            snippet: quote.preview.chars().take(120).collect(),
            deleted: false,
        });
        // Forwarded message: from HTML `schema.skype.com/Forward` block and/or `properties.forwardedMessage`.
        let fwd_prop = forwarded_property(props);
        if parsed.forward.is_some() || fwd_prop.is_some() {
            let prop_str = |keys: &[&str]| -> String {
                fwd_prop
                    .as_ref()
                    .and_then(|v| keys.iter().find_map(|k| v[*k].as_str()).map(str::trim).filter(|s| !s.is_empty()))
                    .unwrap_or("")
                    .to_owned()
            };
            let html_fwd = parsed.forward.as_ref();
            let orig_sender = html_fwd
                .map(|f| f.name.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| prop_str(&["originalSenderDisplayName", "originalSenderName", "senderName"]));
            let orig_conv_id = prop_str(&["originalConversationId", "sourceThreadId", "conversationId"]);
            let orig_conv_title = html_fwd
                .map(|f| f.conversation.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    let t = prop_str(&["originalConversationTitle", "sourceConversationName", "conversationTitle"]);
                    (!t.is_empty()).then_some(t)
                })
                .or_else(|| {
                    (!orig_conv_id.is_empty())
                        .then(|| self.ctx.known(&orig_conv_id).ok().flatten().map(|r| r.title))
                        .flatten()
                })
                .unwrap_or_default();
            let orig_msg_id = html_fwd
                .map(|f| f.id.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    fwd_prop
                        .as_ref()
                        .map(|v| id_of(&v["originalMessageId"]))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_default()
                });
            let orig_link = html_fwd
                .map(|f| f.link.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    let l = prop_str(&["deepLink", "link", "sourceLink"]);
                    (!l.is_empty()).then_some(l)
                })
                .or_else(|| {
                    (!orig_conv_id.is_empty() && !orig_msg_id.is_empty())
                        .then(|| super::links::message_share(&orig_conv_id, &orig_msg_id, &self.session.tenant, None))
                })
                .unwrap_or_default();
            let fwd_body = html_fwd
                .map(|f| f.preview.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| prop_str(&["originalMessageContent", "content", "preview"]));
            if !fwd_body.is_empty() {
                if message.text.is_empty() {
                    message.text = fwd_body;
                } else if !message.text.contains(&fwd_body) {
                    message.text.push('\n');
                    message.text.push_str(&fwd_body);
                }
            }
            let author = orig_sender.clone();
            let (from_title, from_space) = match (orig_sender.is_empty(), orig_conv_title.is_empty()) {
                (false, false) => (orig_sender, orig_conv_title),
                (false, true) => (orig_sender, String::new()),
                (true, false) => (orig_conv_title, String::new()),
                (true, true) => (String::new(), String::new()),
            };
            message.forwarded = Some(Box::new(Forwarded {
                from_title,
                from_space,
                link: orig_link,
                author,
                text: String::new(),
            }));
        }
        message.reactions = self.reactions(&props["emotions"], &me);
        // What cannot be drawn is named and linked, never left blank.
        let cards = listed(&props["cards"]);
        let meeting = set(&props["meeting"]) || props["meeting"].is_object();
        if meeting {
            let meeting = match &props["meeting"] {
                Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
                other => other.clone(),
            };
            let title = [&meeting["meetingtitle"], &meeting["subject"]].into_iter().filter_map(Value::as_str).find(|title| !title.is_empty());
            message.text = title.map_or_else(|| "Meeting".to_owned(), |title| format!("Meeting: {title}"));
            message.spans.clear();
            message.kind = MessageKind::Unsupported {
                link: format!("{TEAMS}/l/meetup-join/{}/0", escaped(conversation)),
            };
        } else if let Some(component) = loop_card(&cards) {
            // A Loop component names itself and opens its file. Live editing is not drawn.
            let label = loop_label(&component.title);
            if message.text.is_empty() {
                message.text = label;
                message.spans.clear();
            } else {
                message.text.push('\n');
                message.text.push_str(&label);
            }
            let link = if component.url.is_empty() { self.message_link(conversation, &id, raw) } else { component.url };
            message.kind = MessageKind::Unsupported { link };
        } else if parsed.card || !cards.is_empty() || kind == "RichText/Media_Card" {
            let link = self.message_link(conversation, &id, raw);
            let mut rendered = super::card::parse_cards(&cards, content, &id, &link, |url| self.pictures(url));
            if !rendered.cards.is_empty() {
                if !rendered.media.is_empty() {
                    if let Err(err) = self.ctx.media(&rendered.media) {
                        tracing::warn!("chatd: Teams: {err}");
                    }
                }
                message.card = Some(Box::new(rendered.cards.remove(0)));
                message.cards = rendered.cards;
                // Keep the message normal (reactable, quotable) when the card rendered.
            } else {
                let title = cards.iter().find_map(|card| card_title(card, 6));
                if message.text.is_empty() {
                    message.text = title.map_or_else(|| "Card".to_owned(), |title| format!("Card: {}", title.chars().take(200).collect::<String>()));
                }
                message.kind = MessageKind::Unsupported { link };
            }
        } else if message.text.is_empty() && message.attachments.is_empty() && message.card.is_none() && message.previews.is_empty() {
            message.text = "Unsupported message".to_owned();
            message.kind = MessageKind::Unsupported { link: self.message_link(conversation, &id, raw) };
        }
        message.client_id = echoed_client_id(raw, message.outgoing, &id);
        Read::Message(Box::new(message))
    }

    /// `properties.emotions`: one entry per reaction, each with everyone who gave it. Teams
    /// keeps every time someone did, so people are counted once.
    fn reactions(&self, emotions: &Value, me: &str) -> Vec<Reaction> {
        let mut found = Vec::new();
        for entry in emotions.as_array().into_iter().flatten() {
            let Some(key) = entry["key"].as_str() else {
                continue;
            };
            let mut people: Vec<&str> = entry["users"].as_array().into_iter().flatten().filter_map(|user| user["mri"].as_str()).collect();
            people.sort_unstable();
            people.dedup();
            if people.is_empty() {
                continue;
            }
            let emoji = emoji_of(key);
            // A key outside the table is remembered, so the user can give the same reaction.
            if key_of(&emoji).is_none() && !emoji.starts_with(':') {
                let note = format!("r:{}", bare_emoji(&emoji));
                if self.state().noted.insert(note.clone()) {
                    self.ctx.set_note(&note, key);
                }
            }
            found.push(Reaction {
                emoji,
                file: None,
                count: people.len() as u32,
                mine: people.contains(&me),
                who: people.iter().take(protocol::REACTION_REACTORS).map(|mri| (*mri).to_owned()).collect(),
            });
        }
        found
    }

    // Push.

    /// A `NewMessage` or `MessageUpdate` event's message.
    pub(super) fn heard(&self, raw: &Value, new: bool) -> Result<(), Fault> {
        let Some((place, root)) = placed(raw) else {
            return Ok(());
        };
        if place == super::tree::ACTIVITY {
            return self.heard_activity(raw, new);
        }
        if raw["messagetype"].as_str() == Some("ThreadActivity/TopicUpdate") {
            let content = raw["content"].as_str().unwrap_or("");
            if let Some(topic) = inside(content, "value")
                .first()
                .map(|t| html::parse(t, &Refs { teams: self, mentions: &[] }, "").text)
                .filter(|t| !t.trim().is_empty())
            {
                let _ = self.rename_row(&place, &topic);
            }
        }
        match self.read(&place, raw) {
            Read::Typing { person, name, on } => {
                if person != self.me() && self.ctx.known(&place)?.is_some() {
                    if on {
                        let name = if name.is_empty() { self.name_of(&person).unwrap_or_default() } else { name };
                        self.ctx.typing(&place, &person, &name, TYPING_SHOWN);
                    } else {
                        self.ctx.typing_stopped(&place, &person);
                    }
                }
                Ok(())
            }
            Read::Deleted(id) => {
                self.ctx.deleted(&place, &id)?;
                self.ctx.deleted(&chain_id(&place, root.as_deref().unwrap_or(&id)), &id)?;
                Ok(())
            }
            Read::Message(message) if new => self.arrived(&place, root.as_deref(), *message),
            Read::Message(message) => self.changed(&place, root.as_deref(), *message),
            Read::Nothing => Ok(()),
        }
    }

    fn arrived(&self, place: &str, root: Option<&str>, mut message: Message) -> Result<(), Fault> {
        if !self.known(place)? {
            return Ok(());
        }
        // A chat the user hid comes back with a new message, as it does in Teams.
        if let Some(row) = self.ctx.known(place)?.filter(|row| row.hidden) {
            let filing = crate::store::Filing { hidden: false, pin: row.pin, ..crate::store::Filing::default() };
            self.ctx.filing(place, &filing)?;
        }
        let Some(root) = root.filter(|root| *root != message.id) else {
            if let Some(stored) = self.ctx.stored_message(place, &message.id)? {
                message.thread = stored.thread;
            }
            self.ctx.message(place, &message)?;
            return Ok(());
        };
        // A reply lives in its chain, not in the channel's own pages.
        let chain = chain_id(place, root);
        self.chain_known(place, root)?;
        let followed = self.followed_chain(place, root)?;
        let unread_reply = !message.outgoing && (message.mentions_me || followed);
        if message.mentions_me || message.outgoing || followed {
            self.ctx.message(&chain, &message)?;
            if unread_reply {
                self.ctx.mark_unread(place)?;
            }
        } else {
            // Teams tells nobody of a reply unless it names them or they follow the post.
            // Opening the chain shows it.
            self.ctx.history(&chain, std::slice::from_ref(&message))?;
            self.ctx.typing_stopped(&chain, &message.sender.id);
        }
        self.bump(place, root, &[&message], unread_reply)
    }

    /// True when I wrote this post or have replied in its chain, matching Teams' default follow rule.
    fn followed_chain(&self, channel: &str, root: &str) -> Result<bool, Fault> {
        let me = self.ctx.key(&self.me());
        if let Some(post) = self.ctx.stored_message(channel, root)? {
            if post.outgoing || post.thread.as_ref().is_some_and(|thread| thread.participants.contains(&me)) {
                return Ok(true);
            }
        }
        let chain = chain_id(channel, root);
        if self.ctx.stored_message(&chain, root)?.is_some_and(|post| post.outgoing) {
            return Ok(true);
        }
        Ok(self.replies_held(&chain, root)?.iter().any(|reply| reply.outgoing))
    }

    /// An edit, or a reaction given or taken back: Teams sends the whole message again. Only
    /// what is stored is updated, wherever it is: the conversation, or the chain it is in.
    fn changed(&self, place: &str, root: Option<&str>, message: Message) -> Result<(), Fault> {
        for conversation in [place.to_owned(), chain_id(place, root.unwrap_or(&message.id))] {
            let Some(stored) = self.ctx.stored_message(&conversation, &message.id)? else {
                continue;
            };
            let mut message = message.clone();
            message.thread = stored.thread;
            message.client_id = stored.client_id;
            message.reply_to = message.reply_to.or(stored.reply_to);
            self.ctx.history(&conversation, &[message])?;
        }
        Ok(())
    }

    /// The summary a post shows of its chain, after these replies: each one newer than the
    /// last counted adds one, so reading the same page twice counts nothing twice.
    fn summary(&self, channel: &str, root: &str, before: Option<ThreadSummary>, replies: &[&Message]) -> Option<ThreadSummary> {
        let mut summary = before.or_else(|| {
            (!replies.is_empty()).then(|| ThreadSummary {
                id: chain_id(channel, root),
                replies: 0,
                last: 0,
                participants: Vec::new(),
                unread: 0,
            })
        })?;
        let mut newer: Vec<&&Message> = replies.iter().filter(|reply| reply.sent > summary.last || summary.replies == 0).collect();
        newer.sort_by_key(|reply| (reply.sent, millis(&reply.id)));
        for reply in newer {
            summary.replies += 1;
            summary.last = summary.last.max(reply.sent);
            let who = self.ctx.key(&reply.sender.id);
            summary.participants.retain(|known| *known != who);
            summary.participants.insert(0, who);
        }
        summary.participants.truncate(protocol::THREAD_FACES);
        Some(summary)
    }

    /// Replies arrived for a post that is stored: its summary counts them.
    fn bump(&self, channel: &str, root: &str, replies: &[&Message], unread: bool) -> Result<(), Fault> {
        let Some(mut first) = self.ctx.stored_message(channel, root)? else {
            return Ok(());
        };
        let before_replies = first.thread.as_ref().map_or(0, |thread| thread.replies);
        let mut after = self.summary(channel, root, first.thread.clone(), replies);
        if unread {
            if let Some(summary) = after.as_mut() {
                let added = summary.replies.saturating_sub(before_replies);
                if added > 0 {
                    summary.unread = summary.unread.saturating_add(added);
                }
            }
        }
        if after != first.thread {
            first.thread = after;
            self.ctx.history(channel, &[first])?;
        }
        Ok(())
    }

    /// Clears `ThreadSummary.unread` on the parent post when its reply chain is marked read.
    pub(super) fn clear_chain_unread(&self, channel: &str, root: &str) -> Result<(), Fault> {
        let Some(mut first) = self.ctx.stored_message(channel, root)? else {
            return Ok(());
        };
        if let Some(thread) = first.thread.as_mut() {
            if thread.unread > 0 {
                thread.unread = 0;
                self.ctx.history(channel, &[first])?;
            }
        }
        Ok(())
    }

    // Pages.

    /// One page of a conversation, newest first, and the cursor of the page before it.
    pub(super) fn page(&self, conversation: &str, size: u32, window: &str) -> Result<(Vec<Value>, String), Fault> {
        let path = format!(
            "/v1/users/ME/conversations/{}/messages?view=msnp24Equivalent&pageSize={size}&{window}\
             &targetType=Passport%7CSkype%7CLync%7CThread%7CPSTN%7CAgent",
            escaped(conversation)
        );
        let mut body = self.chat("GET", &path, Body::None, true)?.body;
        let cursor = |link: &Value| {
            let query = link.as_str()?.split_once('?')?.1;
            query.split('&').find_map(|pair| pair.strip_prefix("syncState=")).map(str::to_owned)
        };
        let older = cursor(&body["_metadata"]["backwardLink"]).unwrap_or_default();
        let messages = match body.get_mut("messages").map(Value::take) {
            Some(Value::Array(messages)) => messages,
            _ => Vec::new(),
        };
        Ok((messages, older))
    }

    /// Records a page. In a channel the posts are its own and each reply goes to its chain; a
    /// chain, a chat and anything else holds every message itself. `more_before` is true when
    /// Teams gave a cursor for a still older page.
    fn record(&self, conversation: &str, raws: &[Value], more_before: bool) -> Result<Vec<Message>, Fault> {
        let channel = chain_of(conversation).is_none() && self.ctx.known(conversation)?.is_some_and(|row| row.kind == ConversationKind::Channel);
        let (mut own, mut replies): (Vec<Message>, Vec<(String, Message)>) = (Vec::new(), Vec::new());
        for raw in raws {
            let root = placed(raw).and_then(|(_, root)| root);
            match self.read(conversation, raw) {
                Read::Message(message) => match root.filter(|root| channel && *root != message.id) {
                    Some(root) => replies.push((root, *message)),
                    None => own.push(*message),
                },
                Read::Deleted(id) => self.ctx.deleted(conversation, &id)?,
                Read::Typing { .. } | Read::Nothing => {}
            }
        }
        let of = |root: &str| -> Vec<&Message> { replies.iter().filter(|(of, _)| of == root).map(|(_, reply)| reply).collect() };
        if channel {
            for post in &mut own {
                let before = self.ctx.stored_message(conversation, &post.id)?.and_then(|stored| stored.thread);
                post.thread = self.summary(conversation, &post.id, before, &of(&post.id));
            }
        }
        self.ctx.history(conversation, &own)?;
        if let Some((parent, root)) = chain_of(conversation) {
            let on_page = own.iter().filter(|message| message.id != root).count();
            // A short page with no older cursor is the start of the chain. The count is every
            // reply stored, not only this page, so the last page of a long chain does not
            // replace the total with itself. No client names a reply-count field on the post.
            if !more_before && on_page < CHAIN as usize - 1 {
                self.exact_chain(parent, root)?;
            }
        }
        // A chain named "Replies" before its post was read is titled by the post now.
        if channel {
            for post in own.iter().filter(|post| post.thread.is_some()) {
                if self.ctx.known(&chain_id(conversation, &post.id))?.is_some() {
                    self.chain_known(conversation, &post.id)?;
                }
            }
        }
        let mut roots: Vec<&str> = replies.iter().map(|(root, _)| root.as_str()).collect();
        roots.sort_unstable();
        roots.dedup();
        for root in roots {
            self.chain_known(conversation, root)?;
            let answers: Vec<Message> = of(root).into_iter().cloned().collect();
            self.ctx.history(&chain_id(conversation, root), &answers)?;
            // A post older than this page still counts what was just seen of its chain.
            if !own.iter().any(|post| post.id == root) {
                self.bump(conversation, root, &of(root), false)?;
            }
        }
        Ok(own)
    }

    /// The newest page. A reply chain keeps the cursor, the same way a chat does, so scrolling
    /// up reads the pages under it.
    pub(super) fn backfill(&self, conversation: &str) -> Result<(), Fault> {
        if !self.known(conversation)? {
            return Ok(());
        }
        if let Some((channel, root)) = chain_of(conversation) {
            self.chain_known(channel, root)?;
            let (messages, older) = self.page(conversation, CHAIN, "startTime=1")?;
            self.remember_cursor(conversation, &messages, &older);
            self.record(conversation, &messages, !older.is_empty())?;
            return Ok(());
        }
        let (messages, older) = self.page(conversation, PAGE, "startTime=1")?;
        self.remember_cursor(conversation, &messages, &older);
        let own = self.record(conversation, &messages, !older.is_empty())?;
        // Refine Teams' boolean unread flag into an exact count against the consumption horizon.
        if let Some(row) = self.ctx.known(conversation)? {
            if row.unread > 0 || row.last_read.is_some() {
                let (unread, mentions) = count_unread_against_horizon(&own, row.last_read.as_deref());
                if unread > row.unread || mentions > row.mentions {
                    self.ctx.counts(
                        conversation,
                        unread.max(row.unread),
                        mentions.max(row.mentions),
                        row.last_read.as_deref(),
                        row.updated,
                    )?;
                }
            }
        }
        self.seen_by(conversation)
    }

    /// Every reply stored in a chain, once its first page has been reached.
    fn exact_chain(&self, parent: &str, root: &str) -> Result<(), Fault> {
        let Some(mut first) = self.ctx.stored_message(parent, root)? else {
            return Ok(());
        };
        let replies = self.replies_held(&chain_id(parent, root), root)?;
        let refs: Vec<&Message> = replies.iter().collect();
        let mut exact = self.summary(parent, root, None, &refs);
        if let (Some(summary), Some(prev)) = (exact.as_mut(), first.thread.as_ref()) {
            summary.unread = prev.unread;
        }
        if exact != first.thread {
            first.thread = exact;
            self.ctx.history(parent, &[first])?;
        }
        Ok(())
    }

    fn replies_held(&self, conversation: &str, root: &str) -> Result<Vec<Message>, Fault> {
        let newest = self.ctx.newest(conversation)?.unwrap_or_default();
        if newest.is_empty() {
            return Ok(Vec::new());
        }
        let ids = self.ctx.stored_ids(conversation, "", &newest, CHAIN.saturating_mul(4))?;
        let mut replies = Vec::new();
        for id in ids.into_iter().filter(|id| id != root) {
            if let Some(message) = self.ctx.stored_message(conversation, &id)? {
                replies.push(message);
            }
        }
        Ok(replies)
    }

    /// Where the page before the oldest message of this one starts, for when it is asked for.
    fn remember_cursor(&self, conversation: &str, messages: &[Value], older: &str) {
        let oldest = messages.iter().map(|raw| id_of(&raw["id"])).filter(|id| millis(id) > 0).min_by_key(|id| millis(id));
        if let Some(oldest) = oldest.filter(|_| !older.is_empty()) {
            self.ctx.set_note(&format!("older:{conversation}"), &format!("{oldest} {older}"));
        }
    }

    /// The page before `before`: by the cursor Teams gave with the page that ended there, or
    /// by time when another page was read since. A reply chain uses that same cursor.
    pub(super) fn older(&self, conversation: &str, before: &str) -> Result<(), Fault> {
        if millis(before) == 0 {
            return Ok(());
        }
        let chain = chain_of(conversation).is_some();
        if chain && self.held_older(conversation, before)? {
            return Ok(());
        }
        let note = self.ctx.note(&format!("older:{conversation}")).unwrap_or_default();
        let (messages, older) = self.page(conversation, history_limit(conversation), &older_window(&note, before))?;
        self.remember_cursor(conversation, &messages, &older);
        self.record(conversation, &messages, !older.is_empty())?;
        Ok(())
    }

    /// True when a reply older than `before` is already stored, so this scroll was fetched.
    fn held_older(&self, conversation: &str, before: &str) -> Result<bool, Fault> {
        let ids = self.ctx.stored_ids(conversation, "", before, 8)?;
        Ok(ids.iter().any(|id| id != before && millis(id) > 0 && millis(id) < millis(before)))
    }

    /// The page after `after`, when the cache has a gap there. Unverified: Teams answers the
    /// newest messages since a time, so a gap longer than a page is closed from its new end.
    pub(super) fn after(&self, conversation: &str, after: &str) -> Result<(), Fault> {
        if chain_of(conversation).is_some() || millis(after) == 0 {
            return Ok(());
        }
        let newest = self.ctx.newest(conversation)?.unwrap_or_default();
        let held = self.ctx.stored_ids(conversation, after, &newest, PAGE)?;
        if held.len() >= PAGE as usize {
            return Ok(());
        }
        let (messages, _) = self.page(conversation, PAGE, &format!("startTime={}", millis(after)))?;
        self.record(conversation, &messages, true)?;
        Ok(())
    }

    /// The page that ends just after `around`; what follows it is asked for as it is read.
    /// A reply chain answers too, and keeps the cursor so scrolling up from there continues.
    pub(super) fn around(&self, conversation: &str, around: &str) -> Result<(), Fault> {
        if millis(around) == 0 {
            return Ok(());
        }
        let chain = chain_of(conversation).is_some();
        if chain && self.ctx.stored_message(conversation, around)?.is_some() {
            return Ok(());
        }
        let (messages, older) = self.page(conversation, history_limit(conversation), &around_window(around))?;
        if chain {
            self.remember_cursor(conversation, &messages, &older);
        }
        self.record(conversation, &messages, !older.is_empty())?;
        Ok(())
    }

    // Acting on messages.

    fn own_name(&self) -> String {
        if !self.session.user.is_empty() {
            return self.session.user.clone();
        }
        self.name_of(&self.me()).unwrap_or_default()
    }

    fn body(&self, client_id: &str, content: &str) -> Value {
        // Nothing more: Teams accepts a message with fields it does not expect and drops it.
        json!({
            "clientmessageid": client_id, "content": content, "messagetype": "RichText/Html",
            "contenttype": "text", "imdisplayname": self.own_name(),
        })
    }

    /// The message a reply quotes, as the line shown above it.
    fn quoted(&self, conversation: &str, id: &str) -> Result<Option<ReplyTo>, Fault> {
        Ok(self.ctx.stored_message(conversation, id)?.map(|stored| ReplyTo {
            person: Some(&stored.sender.id).filter(|id| !id.is_empty()).map(|id| self.ctx.key(id)),
            id: stored.id,
            sender: stored.sender.name,
            snippet: stored.text.chars().take(120).collect(),
            deleted: false,
        }))
    }

    /// Posts a message and ends the send. `client_id` goes as Teams' own `clientmessageid`, so
    /// the copy push delivers replaces the pending row. In a chain's conversation it is a reply
    /// in that chain. A reply to a message in a chat quotes it, as Teams does; a reply to a
    /// post in a channel goes into the post's chain and is shown in the channel as a reply.
    pub(super) fn post(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>) {
        self.post_with_outgoing(conversation, client_id, text, reply_to, &protocol::Outgoing::default());
    }

    pub(super) fn post_with_outgoing(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: &protocol::Outgoing) {
        let outcome = self.posted_with(conversation, client_id, text, reply_to, with, None).map_err(super::reason);
        self.ctx.sent(conversation, client_id, outcome);
    }

    /// Asks Teams' own middle-tier unfurl service for link preview metadata.
    /// Never fetches the target URL directly.
    fn unfurl_link(&self, url: &str) -> Option<(Value, Preview)> {
        let place = self.place().ok()?;
        let endpoint = format!("{}/v1/urlpreview", place.middle);
        let body = json!({"url": url});
        let answer = self.call("POST", &endpoint, super::Auth::Both(super::SPACES), Body::Json(&body), false).ok()?;
        let info = &answer.body;
        let title = [&info["title"], &info["previewTitle"]].into_iter().filter_map(Value::as_str).find(|s| !s.is_empty()).unwrap_or("");
        let description = [&info["description"], &info["previewDescription"]].into_iter().filter_map(Value::as_str).find(|s| !s.is_empty()).unwrap_or("");
        if title.is_empty() && description.is_empty() {
            return None;
        }
        let site = [&info["siteName"], &info["site"], &info["domain"]]
            .into_iter()
            .filter_map(Value::as_str)
            .find(|s| !s.is_empty())
            .unwrap_or_else(|| super::https_host(url).unwrap_or(""));
        let mut entry = json!({
            "url": url,
            "title": title,
            "description": description,
            "siteName": site,
        });
        let img_url = [&info["image"], &info["previewImage"], &info["thumbnailUrl"]]
            .into_iter()
            .filter_map(Value::as_str)
            .find(|u| !u.is_empty() && (self.pictures(u) || self.ours(u)));
        let mut media = Vec::new();
        let image = img_url.map(|u| {
            entry["previewImage"] = Value::String(u.to_owned());
            let file = MediaFile::new(format!("preview:sent:{}", escaped(url)), MediaKind::Image);
            media.push((file.clone(), u.to_owned()));
            file
        });
        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }
        let preview = Preview {
            url: url.to_owned(),
            site: site.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            image,
        };
        Some((entry, preview))
    }

    /// The same, with `properties` (the files or links a message names) beside the body.
    pub(super) fn posted_with(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: &protocol::Outgoing, mut properties: Option<Value>) -> Result<Message, Fault> {
        let mention_spans = &with.spans;
        let mut sent_previews = Vec::new();
        let allow_previews = !with.suppress_embeds && self.ctx.hub.settings().on(protocol::settings::LINK_PREVIEWS);
        if allow_previews {
            if let Some(url) = first_url(text) {
                if let Some((entry, preview)) = self.unfurl_link(&url) {
                    let props = properties.get_or_insert_with(|| json!({}));
                    if let Some(map) = props.as_object_mut() {
                        map.insert("links".to_owned(), Value::String(Value::Array(vec![entry]).to_string()));
                    }
                    sent_previews.push(preview);
                }
            }
        }
        let (mut content, mentions_json) = if !mention_spans.is_empty() {
            let (c, m) = html::compose_with_mentions(text, mention_spans);
            (c, (!m.is_empty()).then(|| Value::Array(m).to_string()))
        } else {
            let (c, m) = html::compose_rich(text);
            (c, (!m.is_empty()).then(|| html::mentions_property(&m)))
        };
        if let Some(encoded) = mentions_json {
            let props = properties.get_or_insert_with(|| json!({}));
            if let Some(map) = props.as_object_mut() {
                map.insert("mentions".to_owned(), Value::String(encoded));
            }
        }
        let mut target = conversation.to_owned();
        let quote = match reply_to {
            Some(original) => self.quoted(conversation, original)?,
            None => None,
        };
        let channel = chain_of(conversation).is_none() && self.ctx.known(conversation)?.is_some_and(|row| row.kind == ConversationKind::Channel);
        let post_subject = if channel && reply_to.is_none() {
            with.subject
                .as_deref()
                .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" "))
                .filter(|s| !s.is_empty())
                .map(|mut s| {
                    protocol::clip(&mut s, 200);
                    s
                })
        } else {
            None
        };
        if let Some(ref subject) = post_subject {
            let props = properties.get_or_insert_with(|| json!({}));
            if let Some(map) = props.as_object_mut() {
                map.insert("subject".to_owned(), Value::String(subject.clone()));
            }
        }
        if let Some(level) = importance_wire(with.importance) {
            let props = properties.get_or_insert_with(|| json!({}));
            if let Some(map) = props.as_object_mut() {
                map.insert("importance".to_owned(), Value::String(level.to_owned()));
            }
        }
        match (reply_to, &quote) {
            (Some(original), _) if channel => target = chain_id(conversation, original),
            (_, Some(quote)) => content = format!("{}{content}", quote_markup(quote)),
            _ => {}
        }
        let path = format!("/v1/users/ME/conversations/{}/messages", escaped(&target));
        let mut body = self.body(client_id, &content);
        if let Some(properties) = properties {
            body["properties"] = properties;
        }
        let answer = self.chat("POST", &path, Body::Json(&body), false).map_err(|fault| match fault {
            Fault::Refused { status: 403, .. } if channel && reply_to.is_none() => Fault::Refused {
                status: 403,
                message: "Only channel moderators can start new posts here.".to_owned(),
            },
            other => other,
        })?;
        let id = id_of(&answer.body["OriginalArrivalTime"]);
        if millis(&id) == 0 {
            return Err(Fault::Net("Teams accepted the message but did not name it".to_owned()));
        }
        let me = self.me();
        let name = self.own_name();
        let sent = millis(&id) / 1000;
        let (mut shown, mut spans) = html::styled(text);
        if !mention_spans.is_empty() && shown == text {
            spans.extend(mention_spans.iter().cloned());
        }
        if let Some(ref subject) = post_subject {
            let shift = (subject.len() + usize::from(!shown.is_empty())) as u32;
            for span in &mut spans {
                span.start += shift;
                span.end += shift;
            }
            shown = if shown.is_empty() { subject.clone() } else { format!("{subject}\n{shown}") };
            spans.insert(0, Span { start: 0, end: subject.len() as u32, style: Style::Bold });
            let mut state = self.state();
            if state.own_meta.len() >= super::REMEMBERED {
                state.own_meta.clear();
            }
            state.own_meta.insert(
                (conversation.to_owned(), id.clone()),
                OwnMeta { subject: Some(subject.clone()), images_html: String::new() },
            );
        }
        let mut message = Message::plain(id.clone(), name.clone(), shown, sent, true);
        message.spans = spans;
        message.importance = with.importance;
        message.previews = sent_previews;
        message.sender.avatar = self.picture(&me, &name);
        message.sender.id = me;
        if let Some((channel, root)) = chain_of(&target) {
            if target != conversation {
                self.chain_known(channel, root)?;
                self.ctx.history(&target, std::slice::from_ref(&message))?;
            }
            self.bump(channel, root, &[&message], false)?;
        }
        message.reply_to = quote;
        Ok(message)
    }

    fn message_path(&self, conversation: &str, id: &str) -> String {
        format!("/v1/users/ME/conversations/{}/messages/{}", escaped(conversation), escaped(id))
    }

    pub(super) fn edit(&self, conversation: &str, id: &str, text: &str, mention_spans: &[Span]) -> Result<(), Fault> {
        let raw_id = id.strip_prefix("scheduled:").unwrap_or(id);
        let stored = self.ctx.stored_message(conversation, id)?;
        let (meta, scheduled_at) = {
            let state = self.state();
            (
                state.own_meta.get(&(conversation.to_owned(), raw_id.to_owned())).cloned(),
                state
                    .scheduled
                    .get(raw_id)
                    .copied()
                    .or_else(|| (raw_id != id).then(|| stored.as_ref().map(|m| m.sent)).flatten()),
            )
        };
        let (kept_subject, body_text) = match meta.as_ref().and_then(|meta| meta.subject.as_deref()) {
            Some(subject) => {
                let bold_line = format!("**{subject}**\n");
                let plain_line = format!("{subject}\n");
                let bold_only = format!("**{subject}**");
                if let Some(rest) = text.strip_prefix(&bold_line) {
                    (Some(subject.to_owned()), rest)
                } else if let Some(rest) = text.strip_prefix(&plain_line) {
                    (Some(subject.to_owned()), rest)
                } else if text == bold_only || text == subject {
                    (Some(subject.to_owned()), "")
                } else {
                    (Some(subject.to_owned()), text)
                }
            }
            None => (None, text),
        };
        let prefix = (text.len().saturating_sub(body_text.len())) as u32;
        let rebased_mentions: Vec<Span> = mention_spans
            .iter()
            .filter_map(|span| {
                let start = span.start.checked_sub(prefix)?;
                let end = span.end.checked_sub(prefix)?;
                (end as usize <= body_text.len()).then(|| Span { start, end, style: span.style.clone() })
            })
            .collect();
        let (mut content, mentions_json) = if !rebased_mentions.is_empty() {
            let (c, m) = html::compose_with_mentions(body_text, &rebased_mentions);
            (c, (!m.is_empty()).then(|| Value::Array(m).to_string()))
        } else {
            let (c, m) = html::compose_rich(body_text);
            (c, (!m.is_empty()).then(|| html::mentions_property(&m)))
        };
        if let Some(images_html) = meta.as_ref().map(|meta| meta.images_html.as_str()).filter(|html| !html.is_empty()) {
            content.push_str(images_html);
        }
        // The quote above a reply is part of the body, so it is written again.
        if let Some(quote) = stored.as_ref().and_then(|stored| stored.reply_to.as_ref()).filter(|_| chain_of(conversation).is_none()) {
            content = format!("{}{content}", quote_markup(quote));
        }
        let mut body = self.body(&now_ms().to_string(), &content);
        if mentions_json.is_some() || kept_subject.is_some() || scheduled_at.is_some() {
            let mut props = serde_json::Map::new();
            if let Some(encoded) = mentions_json {
                props.insert("mentions".to_owned(), Value::String(encoded));
            }
            if let Some(subject) = &kept_subject {
                props.insert("subject".to_owned(), Value::String(subject.clone()));
            }
            if let Some(at) = scheduled_at {
                props.insert("scheduledTime".to_owned(), Value::String(super::iso_utc(at)));
            }
            body["properties"] = Value::Object(props);
        }
        self.chat("PUT", &self.message_path(conversation, raw_id), Body::Json(&body), false)?;
        if let Some(mut edited) = stored {
            let (mut shown, mut spans) = html::styled(body_text);
            if !rebased_mentions.is_empty() && shown == body_text {
                spans.extend(rebased_mentions.iter().cloned());
            }
            if let Some(subject) = &kept_subject {
                let shift = (subject.len() + usize::from(!shown.is_empty())) as u32;
                for span in &mut spans {
                    span.start += shift;
                    span.end += shift;
                }
                shown = if shown.is_empty() { subject.clone() } else { format!("{subject}\n{shown}") };
                spans.insert(0, Span { start: 0, end: subject.len() as u32, style: Style::Bold });
            }
            edited.text = shown;
            edited.spans = spans;
            edited.edited = true;
            self.ctx.history(conversation, &[edited])?;
        }
        Ok(())
    }

    pub(super) fn delete(&self, conversation: &str, id: &str) -> Result<(), Fault> {
        let raw_id = id.strip_prefix("scheduled:").unwrap_or(id);
        let path = format!("{}?behavior=softDelete", self.message_path(conversation, raw_id));
        self.chat("DELETE", &path, Body::None, false)?;
        self.state().scheduled.remove(raw_id);
        self.ctx.deleted(conversation, id)?;
        if raw_id != id {
            self.ctx.deleted(conversation, raw_id)?;
        }
        // A post deleted in its channel is gone from the top of its chain too.
        if chain_of(conversation).is_none() {
            self.ctx.deleted(&chain_id(conversation, raw_id), raw_id)?;
        }
        Ok(())
    }

    pub(super) fn react(&self, conversation: &str, id: &str, emoji: &str, on: bool) -> Result<(), Fault> {
        let learned = || self.ctx.note(&format!("r:{}", bare_emoji(emoji)));
        let Some(key) = key_of(emoji).or_else(learned) else {
            return Err(Fault::Refused {
                status: 400,
                message: "tinyclient does not know Teams' name for that emoji yet".to_owned(),
            });
        };
        let path = format!("{}/properties?name=emotions", self.message_path(conversation, id));
        let (method, body) = match on {
            true => ("PUT", json!({"emotions": {"key": key, "value": now_ms()}})),
            false => ("DELETE", json!({"emotions": {"key": key}})),
        };
        self.chat(method, &path, Body::Json(&body), false)?;
        Ok(())
    }

    /// Forwards one or more messages to target conversations on this account, with an optional
    /// note and Teams forward markup referencing original sender, time, link, and any shared
    /// files without re-uploading.
    pub(super) fn forward(
        &self,
        from: &str,
        message_ids: &[String],
        targets: &[String],
        note: &str,
    ) -> Vec<(String, Result<(), String>)> {
        let source_title = self.ctx.known(from).ok().flatten().map(|r| r.title).unwrap_or_default();
        let mut blocks = Vec::new();
        let mut forwarded_files: Vec<Value> = Vec::new();
        let mut first_meta: Option<Value> = None;

        for mid in message_ids {
            let Ok(Some(original)) = self.ctx.stored_message(from, mid) else {
                continue;
            };
            let parent = chain_of(from).map(|(_, root)| root).filter(|r| *r != original.id);
            let link = super::links::message_share(thread_of(from), &original.id, &self.session.tenant, parent);
            blocks.push(forward_markup(&original, &source_title, &link));
            if first_meta.is_none() {
                let sent_ms = match millis(&original.id) {
                    0 => original.sent.saturating_mul(1000),
                    ms => ms,
                };
                first_meta = Some(json!({
                    "originalMessageId": original.id,
                    "originalConversationId": thread_of(from),
                    "originalConversationTitle": source_title,
                    "originalSenderMri": original.sender.id,
                    "originalSenderDisplayName": original.shown_sender(),
                    "originalArrivalTime": sent_ms,
                    "deepLink": link,
                }));
            }
            if let Some(files) = self.state().file_props.get(&(from.to_owned(), mid.clone())).cloned() {
                forwarded_files.extend(files);
            }
        }
        if blocks.is_empty() {
            return targets
                .iter()
                .map(|target| (target.clone(), Err("Original message not found.".to_owned())))
                .collect();
        }
        let mut content = String::new();
        let note = note.trim();
        if !note.is_empty() {
            content.push_str(&html::compose_rich(note).0);
        }
        for block in &blocks {
            content.push_str(block);
        }
        let mut props = serde_json::Map::new();
        if let Some(meta) = first_meta {
            props.insert("forwardedMessage".to_owned(), Value::String(meta.to_string()));
        }
        if !forwarded_files.is_empty() {
            props.insert("files".to_owned(), Value::String(Value::Array(forwarded_files).to_string()));
        }
        let mut results = Vec::with_capacity(targets.len());
        for (ix, target) in targets.iter().enumerate() {
            let client_id = (now_ms().saturating_add(ix as i64)).to_string();
            let mut body = self.body(&client_id, &content);
            if !props.is_empty() {
                body["properties"] = Value::Object(props.clone());
            }
            let path = format!("/v1/users/ME/conversations/{}/messages", escaped(target));
            let res = self.chat("POST", &path, Body::Json(&body), false).map(|_| ()).map_err(super::reason);
            results.push((target.clone(), res));
        }
        results
    }
}

/// How many messages one history fetch asks for. A reply chain is 200; a chat or channel is 50.
fn history_limit(conversation: &str) -> u32 {
    if chain_of(conversation).is_some() { CHAIN } else { PAGE }
}

/// The query for the page before `before`. The cursor Teams sent with the page that ended
/// there wins; otherwise the page ends at that message's time.
fn older_window(note: &str, before: &str) -> String {
    match note.split_once(' ') {
        Some((oldest, cursor)) if oldest == before && !cursor.is_empty() => format!("startTime=1&syncState={cursor}"),
        _ => format!("startTime=1&endTime={}", millis(before) - 1),
    }
}

/// The query for the page that ends just after `around`.
fn around_window(around: &str) -> String {
    format!("startTime=1&endTime={}", millis(around).saturating_add(1))
}

/// The lab's teams-chain-paging driver. Three pages of 200 cover 450 replies, and the page
/// before a message follows the cursor Teams gave with the page that ended there.
pub(super) fn prove_chain_paging() -> Result<(), String> {
    let chain = "19:channel@thread.tacv2;messageid=1760000400400";
    let chat = "19:chat@unq.gbl.spaces";
    if history_limit(chain) != CHAIN || history_limit(chat) != PAGE {
        return Err("a reply chain is not paged at 200".to_owned());
    }
    let mut left = 450u32;
    let mut requests = 0u32;
    let mut note = String::new();
    let mut before = String::new();
    while left > 0 {
        requests += 1;
        if requests > 3 {
            return Err(format!("{left} replies were still unread after three pages"));
        }
        if requests > 1 {
            let window = older_window(&note, &before);
            if !window.contains("syncState=chaincursor") {
                return Err(format!("page {requests} ignored the chain cursor: {window}"));
            }
        }
        let take = left.min(history_limit(chain));
        left -= take;
        before = format!("{}", 1_760_001_000_000u64 + u64::from(left + 1) * 1000);
        if left > 0 {
            note = format!("{before} chaincursor{requests}");
        }
    }
    if requests != 3 {
        return Err(format!("450 replies took {requests} pages"));
    }
    let jump = around_window("1760001010000");
    if jump != "startTime=1&endTime=1760001010001" {
        return Err(format!("a jump into a chain did not ask by time: {jump}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_giphy_gif_plays_from_giphys_small_copy_and_opens_as_the_original() {
        let gif = html::Image { src: "https://media4.giphy.com/media/abc123/giphy.gif?cid=1&rid=giphy.gif".into(), width: 342, height: 250, name: "happy dance".into() };
        let mut media = Vec::new();
        let attached = giphy_gif(&gif, &mut media);
        let thumb = attached.thumb.clone().unwrap();
        assert_eq!((attached.file.kind, attached.mime.as_str(), attached.name.as_str()), (MediaKind::Gif, "image/gif", "happy dance"));
        assert_eq!((thumb.width, thumb.height), (273, 200));
        let urls: Vec<&str> = media.iter().map(|(_, url)| url.as_str()).collect();
        assert_eq!(urls, [gif.src.as_str(), "https://media4.giphy.com/media/abc123/200.webp"]);
        // An address of another shape is shown as it is.
        let odd = html::Image { src: "https://i.giphy.com/abc123.gif".into(), width: 0, height: 0, name: String::new() };
        let mut media = Vec::new();
        let attached = giphy_gif(&odd, &mut media);
        assert_eq!((attached.thumb.as_ref().map(|thumb| thumb.id.clone()), media.len()), (Some(attached.file.id.clone()), 1));
    }

    #[test]
    fn a_copy_of_our_message_keeps_the_client_id_teams_echoes() {
        let raw = json!({"id": "1760001000000", "clientmessageid": "1760000999000111"});
        assert_eq!(echoed_client_id(&raw, true, "1760001000000").as_deref(), Some("1760000999000111"));
        // Teams writes the same id as a number in some payloads.
        let numbered = json!({"id": "1760001000000", "clientmessageid": 1760000999000111u64});
        assert_eq!(echoed_client_id(&numbered, true, "1760001000000").as_deref(), Some("1760000999000111"));
        assert_eq!(echoed_client_id(&raw, false, "1760001000000"), None);
        let same = json!({"id": "1760001000000", "clientmessageid": "1760001000000"});
        assert_eq!(echoed_client_id(&same, true, "1760001000000"), None);
    }

    #[test]
    fn reaction_keys_read_as_emoji_and_back() {
        assert_eq!((emoji_of("like"), emoji_of("heart"), emoji_of("sad")), ("👍".to_owned(), "❤️".to_owned(), "🙁".to_owned()));
        assert_eq!(emoji_of("1f389_partypopper"), "🎉");
        assert_eq!(emoji_of("1f9d1-200d-1f4bb_technologist"), "🧑\u{200d}💻");
        assert_eq!(emoji_of("like-tone3"), "👍🏽");
        assert_eq!(emoji_of("oreoyum"), "(oreoyum)");
        assert_eq!(key_of("👍").as_deref(), Some("like"));
        assert_eq!(key_of("👍🏽").as_deref(), Some("yes-tone3"));
        assert_eq!(key_of("❤").as_deref(), Some("cactuslove"));
        assert_eq!(key_of("❤️").as_deref(), Some("heart"));
        assert_eq!(key_of("🎉").as_deref(), Some("1f389_partypopper"));
        assert_eq!(key_of("🦑").as_deref(), Some("1f991_squid"));
    }

    #[test]
    fn a_catalog_emoji_and_a_skin_tone_use_teams_keys() {
        prove_reactions().unwrap();
    }

    #[test]
    fn a_fluid_embed_card_is_a_loop_component() {
        prove_loop_component().unwrap();
    }

    #[test]
    fn a_shared_picture_is_shown_and_a_document_stays_a_file() {
        assert_eq!(shared_kind("board.png"), MediaKind::Image);
        assert_eq!(shared_kind("board.JPEG"), MediaKind::Image);
        assert_eq!(shared_kind("walk.mp4"), MediaKind::Video);
        assert_eq!(shared_kind("Q4 plan.xlsx"), MediaKind::File);
        assert_eq!(shared_kind("notes"), MediaKind::File);
    }

    #[test]
    fn a_message_says_where_it_is_and_what_chain_it_hangs_off() {
        let reply = json!({"conversationLink": "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations/19:gen@thread.tacv2;messageid=1760000200200"});
        assert_eq!(placed(&reply), Some(("19:gen@thread.tacv2".to_owned(), Some("1760000200200".to_owned()))));
        let chat = json!({"conversationLink": "https://x/v1/users/ME/conversations/19:a_b@unq.gbl.spaces"});
        assert_eq!(placed(&chat), Some(("19:a_b@unq.gbl.spaces".to_owned(), None)));
        assert_eq!(placed(&json!({"conversationid": "19:c@thread.v2"})), Some(("19:c@thread.v2".to_owned(), None)));
        assert_eq!(placed(&json!({})), None);
        assert_eq!(person("https://emea.ng.msg.teams.microsoft.com/v1/users/ME/contacts/8:orgid:ana"), "8:orgid:ana");
        assert_eq!(person("8:orgid:ana"), "8:orgid:ana");
    }

    #[test]
    fn times_teams_writes_three_ways_are_read() {
        assert!(set(&json!("1760000300300")) && set(&json!(1760000300300u64)));
        assert!(!set(&json!("")) && !set(&json!("0")) && !set(&json!(0)) && !set(&Value::Null));
        assert_eq!(listed(&json!("[{\"itemid\":0}]")).len(), 1);
        assert_eq!(listed(&json!([{"itemid": 0}, {"itemid": 1}])).len(), 2);
        assert!(listed(&json!("not json")).is_empty());
    }

    #[test]
    fn system_content_is_read_without_rendering_it() {
        let added = "<addmember><eventtime>1790000000000</eventtime><initiator>8:orgid:a</initiator><target>8:orgid:b</target><target>8:orgid:c</target></addmember>";
        assert_eq!(inside(added, "target"), ["8:orgid:b", "8:orgid:c"]);
        assert_eq!(inside(added, "initiator"), ["8:orgid:a"]);
        assert!(inside("<a>unclosed", "a").is_empty());
        assert_eq!(call_line("<partlist type=\"started\" alt=\"\"></partlist>"), "A call started");
        assert_eq!(call_line("<partlist type=\"ended\"><part identity=\"8:orgid:a\"><duration>125</duration></part><part><duration>90.5</duration></part></partlist>"), "A call ended · 2 min");
        assert_eq!(call_line("<partlist type=\"missed\"></partlist>"), "Missed call");
        let card = json!([{"content": "{\"type\":\"AdaptiveCard\",\"actions\":[{\"title\":\"Open\"}],\"body\":[{\"type\":\"TextBlock\",\"text\":\"Deploy #42\"}]}"}]);
        assert_eq!(card_title(&card, 6).as_deref(), Some("Deploy #42"));
    }

    #[test]
    fn a_quote_is_written_the_way_it_is_read() {
        let reply = ReplyTo {
            id: "1760000100100".into(),
            sender: "Ana <Ruiz>".into(),
            snippet: "is it \"green\"?".into(),
            person: None,
            deleted: false,
        };
        struct None_;
        impl Lookup for None_ {
            fn mention(&self, _: &str) -> Option<(MentionKind, String)> {
                None
            }
            fn hosted(&self, _: &str) -> bool {
                false
            }
            fn emoji(&self, _: &str, _: &str) -> Option<MediaFile> {
                None
            }
        }
        let parsed = html::parse(&format!("{}<p>yes</p>", quote_markup(&reply)), &None_, "");
        let quote = parsed.quote.unwrap();
        assert_eq!(parsed.text, "yes");
        assert_eq!((quote.id.as_str(), quote.name.as_str(), quote.preview.as_str()), ("1760000100100", "Ana <Ruiz>", "is it \"green\"?"));
    }

    #[test]
    fn documented_links_round_trip_to_the_chat_channel_message_and_team() {
        let tenant = "11111111-1111-4111-8111-111111111111";
        let channel = "19:platform0general@thread.tacv2";
        let chat = "19:grp1@thread.v2";
        let group = "e0873025-0000-4000-8000-00000000beef";
        let chat_link = super::super::links::chat_share(chat);
        let channel_link = super::super::links::channel_share(channel, "General", group, tenant);
        let message_link = super::super::links::message_share(channel, "1760000210210", tenant, Some("1760000200200"));
        let team_link = super::super::links::team_share(channel, group, tenant);
        assert_eq!(chat_link, "https://teams.microsoft.com/l/chat/19%3Agrp1%40thread.v2/conversations");
        assert_eq!(
            channel_link,
            format!("https://teams.microsoft.com/l/channel/19%3Aplatform0general%40thread.tacv2/General?groupId={group}&tenantId={tenant}")
        );
        assert_eq!(
            message_link,
            format!("https://teams.microsoft.com/l/message/19%3Aplatform0general%40thread.tacv2/1760000210210?tenantId={tenant}&parentMessageId=1760000200200")
        );
        assert_eq!(
            team_link,
            format!("https://teams.microsoft.com/l/team/19%3Aplatform0general%40thread.tacv2/conversations?groupId={group}&tenantId={tenant}")
        );
        assert_eq!(super::super::links::parse_share(&chat_link).map(|found| found.conversation), Some(chat.to_owned()));
        let channel_back = super::super::links::parse_share(&channel_link).expect("channel");
        assert_eq!(
            (channel_back.kind, channel_back.conversation.as_str(), channel_back.group.as_str(), channel_back.title.as_str()),
            (super::super::links::ShareKind::Channel, channel, group, "General")
        );
        let message_back = super::super::links::parse_share(&message_link).expect("message");
        assert_eq!((message_back.kind, message_back.message.as_str(), message_back.parent.as_str()), (super::super::links::ShareKind::Message, "1760000210210", "1760000200200"));
        let team_back = super::super::links::parse_share(&team_link).expect("team");
        assert_eq!((team_back.kind, team_back.group.as_str()), (super::super::links::ShareKind::Team, group));
        // A chat message uses the documented context. A post's own id is not a parent.
        let in_chat = super::super::links::message_share(chat, "1563480968434", tenant, None);
        assert!(in_chat.contains("context=%7B%22contextType%22%3A%22chat%22%7D"), "{in_chat}");
        assert_eq!(super::super::links::parse_share(&in_chat).map(|found| found.conversation), Some(chat.to_owned()));
        let post = super::super::links::message_share(channel, "1760000200200", tenant, Some("1760000200200"));
        assert!(!post.contains("parentMessageId"), "{post}");
        // The desktop scheme opens the same place. A link that is not Teams does not.
        let desktop = chat_link.replacen("https://teams.microsoft.com", "msteams:", 1);
        assert_eq!(super::super::links::parse_share(&desktop).map(|found| (found.kind, found.conversation)), Some((super::super::links::ShareKind::Chat, chat.to_owned())));
        assert!(super::super::links::parse_share("https://ex.com/plan").is_none());
        assert!(super::super::links::parse_share("https://teams.microsoft.com/l/chat/0/0?users=a@contoso.com").is_none());
    }

    #[test]
    fn importance_maps_to_and_from_teams_wire_values() {
        assert_eq!(importance_of(&Value::Null), protocol::Importance::Normal);
        assert_eq!(importance_of(&json!("standard")), protocol::Importance::Normal);
        assert_eq!(importance_of(&json!("high")), protocol::Importance::Important);
        assert_eq!(importance_of(&json!("HIGH")), protocol::Importance::Important);
        assert_eq!(importance_of(&json!("urgent")), protocol::Importance::Urgent);
        assert_eq!(importance_wire(protocol::Importance::Normal), None);
        assert_eq!(importance_wire(protocol::Importance::Important), Some("high"));
        assert_eq!(importance_wire(protocol::Importance::Urgent), Some("urgent"));
    }

    #[test]
    fn link_previews_and_m365_cards_are_parsed_with_safe_image_filtering() {
        let props = json!({
            "links": json!([
                {
                    "url": "https://example.com/post",
                    "title": "Release Notes",
                    "description": "What shipped today",
                    "siteName": "Example",
                    "previewImage": "https://eu-api.asm.skype.com/v1/objects/abc/views/imgo"
                },
                {
                    "url": "https://evil.example/track",
                    "title": "External Img",
                    "description": "Should not register external image",
                    "previewImage": "https://evil.example/pixel.png"
                }
            ]).to_string(),
            "urlpreviews": [
                {
                    "url": "https://contoso.sharepoint.com/:x:/s/Platform/EaBcDeF",
                    "fileName": "Q4 Budget.xlsx",
                    "fileType": "xlsx",
                    "description": "Platform team spreadsheet"
                }
            ]
        });
        let mut media = Vec::new();
        let previews = parse_link_previews(&props, "1760000900000", |u| u.contains("asm.skype.com"), &mut media);
        assert_eq!(previews.len(), 3);
        assert_eq!((previews[0].title.as_str(), previews[0].site.as_str()), ("Release Notes", "Example"));
        assert!(previews[0].image.is_some());
        assert_eq!(media.len(), 1);
        assert_eq!(previews[1].title, "External Img");
        assert!(previews[1].image.is_none(), "non-Teams image host must not be registered");
        assert_eq!((previews[2].title.as_str(), previews[2].site.as_str()), ("Q4 Budget.xlsx", "Excel · SharePoint"));
        assert_eq!(first_url("Check out https://example.com/release!"), Some("https://example.com/release".to_owned()));
        assert_eq!(first_url("no links here"), None);
    }

    #[test]
    fn unread_count_counts_all_non_outgoing_messages_past_consumption_horizon() {
        let mut msgs = Vec::new();
        for i in 1..=8u64 {
            let mut m = Message::plain(format!("{}", 1_760_000_000_000 + i * 1000), "Ana", format!("msg {i}"), 1_760_000_000 + i as i64, false);
            if i == 5 || i == 7 {
                m.mentions_me = true;
            }
            msgs.push(m);
        }
        // Add an outgoing message past the horizon — must not count as unread.
        msgs.push(Message::plain("1760000009000", "Me", "ok", 1_760_000_009, true));

        // Horizon at msg 1 -> 7 unread messages (2..=8), 2 mentions (5 and 7).
        assert_eq!(count_unread_against_horizon(&msgs, Some("1760000001000")), (7, 2));
        // Horizon at msg 8 -> 0 unread.
        assert_eq!(count_unread_against_horizon(&msgs, Some("1760000008000")), (0, 0));
        // No horizon -> counts only non-outgoing messages newer than latest outgoing (9000) -> 0.
        assert_eq!(count_unread_against_horizon(&msgs, None), (0, 0));
    }
}
