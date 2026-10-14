//! One Slack message, from the socket or from a page of history, as the one message model.

use super::emoji;
use super::text::{Custom, Names, Rich};
use super::{fnv, seconds, thread_id, Slack};
use protocol::{
    Attachment, Card, Forwarded, MediaFile, MediaKind, Message, MessageKind, Preview, Reaction,
    Sender, Style, ThreadSummary, CARD_FIELDS, THREAD_FACES, WAVEFORM_LIMIT,
};
use serde_json::Value;

/// Subtypes Slack words itself as one line about the conversation: "@Ana has joined the channel".
const SYSTEM: &[&str] = &[
    "channel_join", "channel_leave", "channel_topic", "channel_purpose", "channel_name",
    "channel_archive", "channel_unarchive", "channel_convert_to_private",
    "channel_convert_to_public", "channel_posting_permissions", "group_join", "group_leave",
    "group_topic", "group_purpose", "group_name", "group_archive", "group_unarchive",
    "pinned_item", "unpinned_item", "bot_add", "bot_remove", "joiner_notification",
    "joiner_notification_for_inviter", "reminder_add", "ekm_access_denied",
    "channel_canvas_updated", "huddle_thread", "sh_room_created", "sh_room_shared",
];

/// The sizes Slack makes of an uploaded picture, by longest side: `thumb_360` and so on. One
/// that would be larger than the picture itself is not made.
const THUMBS: &[u32] = &[360, 480, 720, 960, 1024];
/// The size a row normally shows: a picture is drawn at most 400 by 300 points, so this is
/// enough at 1x. The other sizes are registered beside it for a screen that needs more.
const SHOWN: u32 = 480;
/// A picture no larger than this is itself registered as the largest size of its thumbnail.
const WHOLE: u32 = 2048;

/// A link preview's text is a summary, not the article.
const DESCRIPTION: usize = 400;

type Registered = Vec<(MediaFile, String)>;
/// The other sizes of registered pictures: the file's id, then each longest side and address.
type Sizes = Vec<(String, Vec<(u32, String)>)>;

/// The accent Slack gave an attachment: a hex colour, or one of its three names.
fn colour(value: &Value) -> Option<u32> {
    match value.as_str()? {
        "good" => Some(0x2e_b886),
        "warning" => Some(0xda_a038),
        "danger" => Some(0xa3_0200),
        hex => u32::from_str_radix(hex.trim_start_matches('#'), 16).ok().filter(|_| hex.trim_start_matches('#').len() == 6),
    }
}

fn first<'a>(values: &[&'a Value]) -> Option<&'a str> {
    values.iter().filter_map(|value| value.as_str()).find(|text| !text.is_empty())
}

fn pixels(value: &Value) -> u32 {
    value.as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32
}

/// A huddle's one line. Slack keeps the same message and marks the room ended.
fn huddle(room: &Value) -> String {
    let (start, end) = (room["date_start"].as_i64().unwrap_or(0), room["date_end"].as_i64().unwrap_or(0));
    if room["has_ended"] == true || end > 0 {
        match (end - start) / 60 {
            minutes if start > 0 && minutes > 0 => format!("A huddle happened · {minutes} min"),
            _ => "A huddle happened".to_owned(),
        }
    } else {
        "A huddle started".to_owned()
    }
}

impl Names for Slack {
    fn me(&self) -> &str {
        self.me.get().map_or("", String::as_str)
    }

    fn user(&self, id: &str) -> String {
        self.user(id).name
    }

    fn channel(&self, id: &str) -> Option<String> {
        if let Some(known) = self.ctx.known(id).ok().flatten() {
            return Some(known.title);
        }
        let note = format!("c:{id}");
        if let Some(title) = self.ctx.note(&note) {
            return Some(title);
        }
        // A channel Slack will not name (private, and not mine) is asked about once a session.
        if self.state().unknown.contains(id) {
            return None;
        }
        let named = self.api("conversations.info", &[("channel", id)]).ok();
        let Some(name) = named.as_ref().and_then(|info| info["channel"]["name"].as_str()) else {
            self.unknown(id);
            return None;
        };
        let title = format!("#{name}");
        self.ctx.set_note(&note, &title);
        Some(title)
    }

    fn usergroup(&self, id: &str) -> Option<String> {
        let note = format!("g:{id}");
        if let Some(handle) = self.ctx.note(&note) {
            return Some(handle);
        }
        // One listing a run names every group; they are few.
        if std::mem::replace(&mut self.state().groups_listed, true) {
            return None;
        }
        let listed = self.api("usergroups.list", &[]).ok()?;
        for group in listed["usergroups"].as_array().into_iter().flatten() {
            if let (Some(id), Some(handle)) = (group["id"].as_str(), group["handle"].as_str()) {
                self.ctx.set_note(&format!("g:{id}"), handle);
            }
        }
        self.ctx.note(&note)
    }

    fn custom(&self, name: &str) -> Option<Custom> {
        let note = format!("e:{name}");
        let known = self.ctx.note(&note).or_else(|| {
            self.list_emoji();
            self.ctx.note(&note)
        })?;
        if let Some(emoji) = known.strip_prefix('=') {
            return Some(Custom::Unicode(emoji.to_owned()));
        }
        let file = MediaFile::new(format!("emoji:{name}:{:x}", fnv(&known)), MediaKind::Emoji);
        if let Err(err) = self.ctx.media(&[(file.clone(), known)]) {
            eprintln!("chatd: Slack: {err}");
        }
        Some(Custom::File(file))
    }
}

impl Slack {
    /// The workspace's custom emoji, written to the store once a day at most and only when a
    /// message first uses a name that is not a standard one. Nothing of it stays in memory.
    fn list_emoji(&self) {
        let now = crate::hub::now();
        let listed = self.ctx.note("emoji").and_then(|at| at.parse::<i64>().ok());
        if listed.is_some_and(|at| now - at < 86_400) {
            return;
        }
        self.ctx.set_note("emoji", &now.to_string());
        let Ok(list) = self.api("emoji.list", &[]) else {
            return;
        };
        let Some(all) = list["emoji"].as_object() else {
            return;
        };
        for (name, value) in all {
            let mut value = value.as_str().unwrap_or("");
            // `alias:other` names another custom emoji or a standard one.
            for _ in 0..3 {
                match value.strip_prefix("alias:") {
                    Some(other) => match emoji::standard(other) {
                        Some(standard) => {
                            self.ctx.set_note(&format!("e:{name}"), &format!("={standard}"));
                            value = "";
                        }
                        None => value = all.get(other).and_then(Value::as_str).unwrap_or(""),
                    },
                    None => break,
                }
            }
            if self.hosted(value) {
                self.ctx.set_note(&format!("e:{name}"), value);
            }
        }
    }

    /// One custom emoji changed: `emoji_changed` on the socket.
    pub(super) fn emoji_changed(&self, event: &Value) {
        match event["subtype"].as_str() {
            Some("add") => {
                let (name, value) = (event["name"].as_str().unwrap_or(""), event["value"].as_str().unwrap_or(""));
                if !name.is_empty() && self.hosted(value) {
                    self.ctx.set_note(&format!("e:{name}"), value);
                }
            }
            Some("remove") => {
                for name in event["names"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    self.ctx.set_note(&format!("e:{name}"), "");
                }
            }
            // A rename is rare; the next daily listing picks it up.
            _ => self.ctx.set_note("emoji", "0"),
        }
    }

    /// A reaction's emoji as the model writes it: the emoji itself, or `:name:` with its picture.
    pub(super) fn reaction(&self, name: &str) -> (String, Option<MediaFile>) {
        if let Some(standard) = emoji::standard(name) {
            return (standard, None);
        }
        match self.custom(name) {
            Some(Custom::Unicode(emoji)) => (emoji, None),
            Some(Custom::File(file)) => (format!(":{name}:"), Some(file)),
            None => (format!(":{name}:"), None),
        }
    }

    /// Where the official app shows this message.
    fn permalink(&self, channel: &str, ts: &str, thread: Option<&str>) -> String {
        let link = format!("{}archives/{channel}/p{}", self.session.url, ts.replace('.', ""));
        match thread {
            Some(thread) => format!("{link}?thread_ts={thread}&cid={channel}"),
            None => link,
        }
    }

    fn sender(&self, event: &Value, media: &mut Registered) -> Sender {
        let by_app = event["bot_id"].is_string() || event["subtype"] == "bot_message";
        // An app may post under a name and picture of its own choosing.
        let chosen = first(&[&event["username"]]).filter(|_| by_app);
        if let (Some(id), None) = (event["user"].as_str(), chosen) {
            let user = self.user(id);
            return Sender {
                id: id.to_owned(),
                avatar: user.avatar(),
                bot: user.bot || by_app,
                name: user.name,
            };
        }
        let profile = &event["bot_profile"];
        let icons = [&event["icons"], &profile["icons"]];
        let icon = icons.iter().find_map(|icons| first(&[&icons["image_72"], &icons["image_64"], &icons["image_48"]]));
        let avatar = icon.filter(|icon| self.hosted(icon)).map(|icon| {
            let file = MediaFile {
                width: 72,
                height: 72,
                ..MediaFile::new(format!("bot:{:x}", fnv(icon)), MediaKind::Avatar)
            };
            media.push((file.clone(), icon.to_owned()));
            file
        });
        Sender {
            id: event["user"].as_str().unwrap_or_default().to_owned(),
            name: chosen.or(first(&[&profile["name"]])).unwrap_or("App").to_owned(),
            avatar,
            bot: true,
        }
    }

    /// An upload as an attachment: the file itself and the picture a row shows inline. Both
    /// addresses need the session to fetch (see `Command::Download`).
    ///
    /// The picture a row shows is registered at the size a row normally draws, and every other
    /// size Slack made of it as a variant, so a 2x screen is given one large enough.
    fn file(&self, file: &Value, media: &mut Registered, variants: &mut Sizes) -> Option<Attachment> {
        let id = file["id"].as_str()?;
        let url = file["url_private"].as_str().filter(|url| self.hosted(url))?;
        let mime = file["mimetype"].as_str().unwrap_or("");
        let kind = match mime.split_once('/') {
            Some(("image", "gif")) => MediaKind::Gif,
            Some(("image", _)) => MediaKind::Image,
            Some(("video", _)) => MediaKind::Video,
            Some(("audio", _)) => MediaKind::Audio,
            _ => MediaKind::File,
        };
        let whole = MediaFile {
            width: pixels(&file["original_w"]),
            height: pixels(&file["original_h"]),
            bytes: file["size"].as_u64().unwrap_or(0),
            ..MediaFile::new(id, kind)
        };
        media.push((whole.clone(), url.to_owned()));

        let still = |key: &str, side: u32| {
            let url = file[key].as_str().filter(|url| self.hosted(url))?;
            let (width, height) = (pixels(&file[format!("{key}_w")]), pixels(&file[format!("{key}_h")]));
            let longest = match width.max(height) {
                0 => side,
                longest => longest,
            };
            Some((url.to_owned(), width, height, longest))
        };
        let mut sizes = match kind {
            MediaKind::Image | MediaKind::Gif => THUMBS.iter().filter_map(|side| still(&format!("thumb_{side}"), *side)).collect(),
            MediaKind::Video => still("thumb_video", 0).into_iter().collect(),
            MediaKind::File => still("thumb_pdf", 0).into_iter().collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        // The picture itself, where it is small enough to stand in for a size Slack did not make.
        let longest = whole.width.max(whole.height);
        if kind == MediaKind::Image && longest <= WHOLE && sizes.last().is_some_and(|(.., largest)| *largest < longest) {
            sizes.push((url.to_owned(), whole.width, whole.height, longest));
        }
        let shown = sizes.iter().position(|(.., longest)| *longest >= SHOWN).or(sizes.len().checked_sub(1));
        let thumb = match shown {
            Some(shown) => {
                let (url, width, height, _) = sizes.remove(shown);
                let thumb = MediaFile {
                    width,
                    height,
                    ..MediaFile::new(format!("{id}:thumb"), MediaKind::Image)
                };
                media.push((thumb.clone(), url));
                // After the file itself, which `message` registers first.
                let others = sizes.into_iter().map(|(url, .., longest)| (longest, url)).collect();
                variants.push((thumb.id.clone(), others));
                Some(thumb)
            }
            // Slack makes no smaller copy of a picture that is already small.
            None if kind == MediaKind::Image && (1..=800).contains(&whole.width) => Some(whole.clone()),
            None => None,
        };
        // A voice clip's loudness over time, as Slack samples it: 0 to 100, a hundred or so.
        let samples: Vec<u64> = file["audio_wave_samples"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
        let every = samples.len().div_ceil(WAVEFORM_LIMIT).max(1);
        let waveform = samples.iter().step_by(every).map(|sample| ((*sample).min(100) * 255 / 100) as u8).collect();
        Some(Attachment {
            // Slack has neither spoilers nor stickers.
            spoiler: false,
            sticker: false,
            waveform,
            name: first(&[&file["name"], &file["title"]]).unwrap_or("untitled").to_owned(),
            file: whole,
            thumb,
            mime: mime.to_owned(),
            duration: (file["duration_ms"].as_u64().unwrap_or(0) / 1000) as u32,
        })
    }

    /// Slack's markup as plain words, for a card's description.
    fn words(&self, text: &str) -> String {
        let mut rich = Rich::new(self);
        rich.mrkdwn(text);
        rich.finish();
        rich.text
    }

    /// A link's card as Slack built it. The picture is kept only when Slack serves it.
    fn preview(&self, card: &Value, url: &str, media: &mut Registered) -> Preview {
        let wide = first(&[&card["image_url"]]).map(|url| (url, &card["image_width"], &card["image_height"]));
        let small = first(&[&card["thumb_url"]]).map(|url| (url, &card["thumb_width"], &card["thumb_height"]));
        let image = wide.or(small).filter(|(url, ..)| self.hosted(url)).map(|(url, width, height)| {
            let file = MediaFile {
                width: pixels(width),
                height: pixels(height),
                ..MediaFile::new(format!("preview:{:x}", fnv(url)), MediaKind::Image)
            };
            media.push((file.clone(), url.to_owned()));
            file
        });
        let description = self.words(first(&[&card["text"], &card["fallback"]]).unwrap_or(""));
        Preview {
            url: url.to_owned(),
            site: first(&[&card["service_name"], &card["author_name"]]).unwrap_or("").to_owned(),
            title: first(&[&card["title"]]).unwrap_or("").to_owned(),
            description: description.chars().take(DESCRIPTION).collect(),
            image,
        }
    }

    /// A card with no link is a bot's own message (a build result, an alert), drawn as one
    /// framed card under the text. What Slack shows above the frame (`pretext`) stays text.
    /// `None` when the card says nothing in a form a `Card` holds.
    // CONTRACT: the store previews a message by its text or first attachment, so a row whose
    // newest message is only a card previews as nothing.
    fn framed(&self, card: &Value, rich: &mut Rich) -> Option<Card> {
        let words = |value: &Value| self.words(value.as_str().unwrap_or(""));
        let fields = card["fields"].as_array().into_iter().flatten().take(CARD_FIELDS);
        let mut framed = Card {
            title: words(&card["title"]),
            text: words(&card["text"]),
            fields: fields.map(|field| (words(&field["title"]), words(&field["value"]))).collect(),
            footer: words(&card["footer"]),
            color: colour(&card["color"]),
        };
        if framed.title.is_empty() && framed.text.is_empty() && framed.fields.is_empty() {
            framed.text = words(&card["fallback"]);
        }
        if framed.title.is_empty() && framed.text.is_empty() && framed.fields.is_empty() {
            return None;
        }
        if let Some(pretext) = first(&[&card["pretext"]]) {
            rich.line();
            rich.mrkdwn(pretext);
        }
        Some(framed)
    }

    /// A message shared from elsewhere ("Share message"): where it came from, and what it said
    /// as the message's own text and files.
    // CONTRACT: `Forwarded` keeps no place for what the forwarder wrote themselves, apart from
    // what was forwarded: both are the message's text, the forwarder's words first.
    fn forwarded(&self, card: &Value, rich: &mut Rich, media: &mut Registered, variants: &mut Sizes, attachments: &mut Vec<Attachment>) -> Forwarded {
        rich.line();
        let blocks = card["message_blocks"][0]["message"]["blocks"].as_array();
        if !blocks.is_some_and(|blocks| rich.blocks(blocks)) {
            rich.mrkdwn(first(&[&card["text"], &card["fallback"]]).unwrap_or(""));
        }
        for file in card["files"].as_array().into_iter().flatten() {
            attachments.extend(self.file(file, media, variants));
        }
        // From the cache alone: a forward is not worth a lookup.
        let place = card["channel_id"].as_str().and_then(|id| self.ctx.known(id).ok().flatten());
        let from_title = match (place, first(&[&card["channel_name"]]), first(&[&card["author_name"]])) {
            (Some(place), ..) => place.title,
            (None, Some(name), _) => format!("#{}", name.trim_start_matches('#')),
            (None, None, Some(author)) => author.to_owned(),
            (None, None, None) => String::new(),
        };
        Forwarded {
            from_title,
            // Another workspace's name is not known here, and Slack does not send it.
            from_space: String::new(),
            link: first(&[&card["from_url"]]).unwrap_or("").to_owned(),
        }
    }

    /// A second card of the same message, or one built of blocks: its words join the text so
    /// nothing a bot said is lost.
    // CONTRACT: a message holds one `Card`; Slack allows many attachments.
    fn card(&self, card: &Value, rich: &mut Rich) {
        let before = rich.text.len();
        if let Some(pretext) = first(&[&card["pretext"]]) {
            rich.line();
            rich.mrkdwn(pretext);
        }
        if let Some(blocks) = card["blocks"].as_array() {
            rich.blocks(blocks);
        }
        if let Some(title) = first(&[&card["title"]]) {
            rich.line();
            let start = rich.text.len() as u32;
            rich.mrkdwn(title);
            let end = rich.text.len() as u32;
            if end > start {
                rich.spans.push(protocol::Span { start, end, style: Style::Bold });
            }
        }
        if let Some(text) = first(&[&card["text"]]) {
            rich.line();
            rich.mrkdwn(text);
        }
        for field in card["fields"].as_array().into_iter().flatten() {
            rich.line();
            if let Some(title) = first(&[&field["title"]]) {
                rich.mrkdwn(title);
                rich.text.push_str(": ");
            }
            rich.mrkdwn(field["value"].as_str().unwrap_or(""));
        }
        if rich.text.len() == before {
            if let Some(fallback) = first(&[&card["fallback"]]) {
                rich.line();
                rich.mrkdwn(fallback);
            }
        }
    }

    /// A message event, or one entry of a page of history. `None` only for what Slack itself
    /// hides. Registers every file the message refers to.
    pub(super) fn message(&self, channel: &str, event: &Value) -> Option<Message> {
        let ts = event["ts"].as_str()?;
        let subtype = event["subtype"].as_str();
        let handled_apart = matches!(subtype, Some("message_changed" | "message_deleted" | "message_replied"));
        if handled_apart || event["hidden"] == true {
            return None;
        }
        let thread = event["thread_ts"].as_str();
        let mut media = Registered::new();
        let mut variants = Sizes::new();
        let sender = self.sender(event, &mut media);
        let outgoing = !sender.id.is_empty() && sender.id == Names::me(self);
        let link = self.permalink(channel, ts, thread.filter(|thread| *thread != ts));

        let mut rich = Rich::new(self);
        let system = subtype.is_some_and(|subtype| SYSTEM.contains(&subtype));
        if event["room"].is_object() {
            rich.text = huddle(&event["room"]);
        } else if !event["blocks"].as_array().is_some_and(|blocks| rich.blocks(blocks)) {
            rich.mrkdwn(event["text"].as_str().unwrap_or(""));
        }

        let mut previews = Vec::new();
        let mut attachments = Vec::new();
        let (mut framed, mut forwarded) = (None, None);
        for card in event["attachments"].as_array().into_iter().flatten() {
            if card["is_share"] == true && forwarded.is_none() {
                forwarded = Some(Box::new(self.forwarded(card, &mut rich, &mut media, &mut variants, &mut attachments)));
                continue;
            }
            match first(&[&card["from_url"], &card["original_url"], &card["title_link"]]) {
                Some(url) => previews.push(self.preview(card, url, &mut media)),
                None if framed.is_none() && !card["blocks"].is_array() => match self.framed(card, &mut rich) {
                    Some(card) => framed = Some(Box::new(card)),
                    None => self.card(card, &mut rich),
                },
                None => self.card(card, &mut rich),
            }
        }
        for file in event["files"].as_array().into_iter().flatten() {
            let name = first(&[&file["name"], &file["title"]]).unwrap_or("file");
            match file["mode"].as_str() {
                // Deleted since.
                Some("tombstone") => continue,
                Some("hidden_by_limit") => {
                    rich.line();
                    rich.text.push_str("[A file is past this workspace's history limit]");
                    continue;
                }
                _ => {}
            }
            match self.file(file, &mut media, &mut variants) {
                Some(attachment) => attachments.push(attachment),
                // Kept somewhere Slack does not serve (Drive, Dropbox): a link to it.
                None => {
                    rich.line();
                    let target = first(&[&file["url_private"], &file["permalink"]]).unwrap_or(&link);
                    rich.link(target, Some(name));
                }
            }
        }
        if rich.undrawn && !rich.text.is_empty() {
            // Buttons and menus are not drawn; the official app has them.
            rich.line();
            rich.link(&link, Some("Open in Slack"));
        }
        rich.finish();
        if subtype == Some("me_message") && !rich.text.is_empty() {
            let end = rich.text.len() as u32;
            rich.spans.push(protocol::Span { start: 0, end, style: Style::Italic });
        }

        let mine = Names::me(self);
        let reactions = event["reactions"].as_array().into_iter().flatten().filter_map(|reaction| {
            let (emoji, file) = self.reaction(reaction["name"].as_str()?);
            let users = reaction["users"].as_array();
            Some(Reaction {
                emoji,
                file,
                count: reaction["count"].as_u64().or(users.map(|users| users.len() as u64)).unwrap_or(1) as u32,
                mine: users.is_some_and(|users| users.iter().any(|user| user == mine)),
            })
        });
        let reactions: Vec<Reaction> = reactions.collect();

        let replies = event["reply_count"].as_u64().unwrap_or(0) as u32;
        // Unverified: `reply_users` is taken to list repliers oldest first, so the latest
        // ones are at its end. Slack documents the field and not its order.
        let repliers = event["reply_users"].as_array().into_iter().flatten().filter_map(Value::as_str);
        let repliers: Vec<protocol::Key> = repliers.rev().take(THREAD_FACES).map(|id| self.ctx.key(id)).collect();
        let summary = (thread == Some(ts) && replies > 0).then(|| ThreadSummary {
            participants: repliers,
            // Slack says how many only for a thread the user follows, and only where it sends
            // the thread's own state (`conversations.replies`). 0 otherwise.
            unread: event["unread_count"].as_u64().unwrap_or(0) as u32,
            id: thread_id(channel, ts),
            replies,
            last: seconds(event["latest_reply"].as_str().unwrap_or(ts)),
        });

        let mut message = Message::plain(ts, "", "", seconds(ts), outgoing);
        let muted = self.state().muted.contains(channel);
        message.mentions_me = !outgoing && (rich.mentioned || (rich.broadcast && !muted));
        let empty = rich.text.is_empty() && attachments.is_empty() && previews.is_empty() && framed.is_none();
        // What Slack leaves of a deleted message that a thread still hangs off.
        let tombstone = subtype == Some("tombstone");
        message.kind = if tombstone {
            MessageKind::Normal
        } else if empty {
            // Never blank, never dropped: one line and the way to the real thing.
            let what = if rich.undrawn { "An interactive message" } else { "A message tinychat cannot show yet" };
            rich.text = format!("{what} from {}", sender.name);
            MessageKind::Unsupported { link }
        } else if system || event["room"].is_object() {
            MessageKind::System
        } else {
            MessageKind::Normal
        };
        message.text = rich.text;
        message.spans = rich.spans;
        message.sender = sender;
        message.attachments = attachments;
        message.previews = previews;
        message.reactions = reactions;
        message.thread = summary;
        message.card = framed;
        message.forwarded = forwarded;
        message.edited = event["edited"].is_object();
        message.pinned = event["pinned_to"].as_array().is_some_and(|pinned| !pinned.is_empty());
        if outgoing {
            message.client_id = event["client_msg_id"].as_str().and_then(super::client_id);
        }
        if tombstone {
            message.tombstone();
        }
        let mut sized = variants.iter().filter(|(_, sizes)| !sizes.is_empty());
        let registered = self.ctx.media(&media).and_then(|()| sized.try_for_each(|(id, sizes)| self.ctx.media_variants(id, sizes)));
        if let Err(err) = registered {
            eprintln!("chatd: Slack: {err}");
        }
        Some(message)
    }
}
