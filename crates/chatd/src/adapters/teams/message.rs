//! One Teams message, from push or from a page of history, as the one message model; and
//! everything done to messages: pages, sending, editing, deleting, reacting.

use super::html::{self, Lookup};
use super::{chain_id, chain_of, escaped, id_of, millis, now_ms, seconds, Body, Fault, Teams, CHAIN, PAGE, TEAMS, TYPING_SHOWN};
use protocol::{
    Attachment, ConversationKind, MediaFile, MediaKind, MentionKind, Message, MessageKind, Reaction,
    ReplyTo, Span, Style, ThreadSummary,
};
use serde_json::{json, Value};

/// Reaction keys and the emoji they are, from Teams' own emoji catalog: the six every client
/// offers first, then the ones people reach for. A key shaped `1f389_partypopper` says its own
/// emoji and needs no entry to be read, only to be sent.
const REACTIONS: &[(&str, &str)] = &[
    ("like", "👍"), ("heart", "❤️"), ("laugh", "😆"), ("surprised", "😮"), ("sad", "🙁"), ("angry", "😠"),
    ("cry", "😢"), ("cwl", "😂"), ("1f389_partypopper", "🎉"), ("praying", "🙏"), ("1f440_eyes", "👀"),
    ("fire", "🔥"), ("2705_whiteheavycheckmark", "✅"), ("sarcastic", "👏"), ("smileeyes", "😊"),
    ("wink", "😉"), ("hearteyes", "😍"), ("think", "🤔"), ("1f4af_hundredpointssymbol", "💯"),
    ("launch", "🚀"), ("no", "👎"), ("rofl", "🤣"), ("cool", "😎"), ("loudlycrying", "😭"),
    ("party", "🥳"), ("1f44b_wavinghand", "👋"), ("muscle", "💪"), ("2728_sparkles", "✨"),
    ("star", "⭐"), ("274c_crossmark", "❌"), ("handshake", "🤝"), ("ok", "👌"), ("shrug", "🤷"),
    ("salute", "🫡"), ("skull", "💀"), ("coffee", "☕"), ("yes", "👍"),
];

/// What reactions a Teams conversation takes: the emoji Teams has a key for here, and nothing
/// custom. Teams names a reaction by a key of its catalog (`like`, `1f389_partypopper`), and
/// one outside the table has no key chatd can spell; a reaction already on a message brought
/// its own key and can be added to. Teams lets a person give several; no limit on them or on
/// different emoji is documented, so none is set.
pub(crate) fn reaction_rules() -> protocol::ReactionRules {
    let mut only: Vec<String> = Vec::with_capacity(REACTIONS.len());
    for (_, emoji) in REACTIONS {
        if !only.iter().any(|held| held == emoji) {
            only.push((*emoji).to_owned());
        }
    }
    // Plain only: chatd sends a toned one as its plain key, so the tone would not stay.
    protocol::ReactionRules { set: protocol::ReactionSet::Only, only, ..protocol::ReactionRules::default() }
}

/// An emoji without the marks that do not change which one it is.
fn bare_emoji(emoji: &str) -> String {
    emoji.chars().filter(|c| *c != '\u{fe0f}' && !('\u{1f3fb}'..='\u{1f3ff}').contains(c)).collect()
}

/// The emoji a reaction key is. A key Teams added since reads as `:key:`.
fn emoji_of(key: &str) -> String {
    let (key, tone) = key.split_once("-tone").map_or((key, None), |(key, tone)| (key, tone.parse::<u32>().ok()));
    let known = REACTIONS.iter().find(|(known, _)| *known == key).map(|(_, emoji)| (*emoji).to_owned());
    let spelled = || {
        let points = key.split_once('_')?.0;
        let read: Option<String> = points.split('-').map(|hex| u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)).collect();
        read.filter(|read| !read.is_empty() && points.len() >= 4)
    };
    let Some(mut emoji) = known.or_else(spelled) else {
        return format!(":{key}:");
    };
    if let Some(tone) = tone.filter(|tone| (1..=5).contains(tone)).and_then(|tone| char::from_u32(0x1f3fa + tone)) {
        emoji.push(tone);
    }
    emoji
}

/// The key Teams knows an emoji by, for the ones in the table.
fn key_of(emoji: &str) -> Option<&'static str> {
    let wanted = bare_emoji(emoji);
    REACTIONS.iter().find(|(_, known)| bare_emoji(known) == wanted).map(|(key, _)| *key)
}

/// The person id at the end of `https://…/v1/users/ME/contacts/8:orgid:…`, or the id itself.
fn person(from: &str) -> &str {
    from.rsplit('/').next().unwrap_or(from)
}

/// The conversation a message is in and, in a channel, the post its reply chain hangs off:
/// `…/conversations/19:x@thread.tacv2;messageid=1700000000000`.
fn placed(raw: &Value) -> Option<(String, Option<String>)> {
    let link = raw["conversationLink"].as_str().unwrap_or("");
    let named = link.split_once("/conversations/").map(|(_, rest)| rest.split('/').next().unwrap_or(rest));
    let named = named.or(raw["conversationid"].as_str()).filter(|named| !named.is_empty())?;
    Some(match chain_of(named) {
        Some((channel, root)) => (channel.to_owned(), Some(root.to_owned())),
        None => (named.to_owned(), None),
    })
}

/// A value Teams writes as a string, a number, or leaves out: `edittime`, `deletetime`.
fn set(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.is_empty() && text != "0" && text != "null",
        Value::Number(number) => number.as_f64().is_some_and(|number| number > 0.),
        _ => false,
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

/// The header of a quoted reply, as Teams' own client writes it.
fn quote_markup(reply: &ReplyTo) -> String {
    let text = |text: &str| html::compose(text).trim_start_matches("<p>").trim_end_matches("</p>").to_owned();
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

impl Teams {
    /// Where the official app opens one message.
    fn message_link(&self, conversation: &str, id: &str) -> String {
        let chat = if conversation.contains("@thread.tacv2") || conversation.contains("@thread.skype") {
            ""
        } else {
            "&context=%7B%22contextType%22%3A%22chat%22%7D"
        };
        format!("{TEAMS}/l/message/{}/{id}?tenantId={}{chat}", escaped(conversation), escaped(&self.session.tenant))
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
        let mut message = Message::plain(id.clone(), name.clone(), "", sent, from == me);
        message.sender.bot = from.starts_with("28:");
        message.sender.avatar = self.picture(&from, &name);
        message.sender.id = from;
        message.edited = set(&props["edittime"]);
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
                message.kind = MessageKind::Unsupported { link: self.message_link(conversation, &id) };
                message.text = "Meeting recording".to_owned();
                return Read::Message(Box::new(message));
            }
            _ => {}
        }
        let mentions = listed(&props["mentions"]);
        let refs = Refs { teams: self, mentions: &mentions };
        let mut parsed = match kind {
            // Plain text is taken as written, not read for markup.
            "Text" => html::parse(&html::compose(content), &refs, &me),
            _ => html::parse(content, &refs, &me),
        };
        // A channel post's headline is part of what it says.
        let subject = [&props["subject"], &props["title"]].into_iter().filter_map(Value::as_str).find(|subject| !subject.trim().is_empty());
        if let Some(subject) = subject.map(str::trim) {
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
        for shared in listed(&props["files"]) {
            let info = &shared["fileInfo"];
            let urls = [&info["shareUrl"], &shared["objectUrl"], &info["fileUrl"]];
            let Some(url) = urls.into_iter().filter_map(Value::as_str).find(|url| url.starts_with("https://")) else {
                continue;
            };
            let named = [&shared["fileName"], &shared["title"]].into_iter().filter_map(Value::as_str).find(|name| !name.is_empty()).unwrap_or("File");
            let sizes = [&shared["fileSize"], &shared["size"], &info["fileSize"]];
            let key = Some(id_of(&shared["id"])).filter(|key| !key.is_empty()).unwrap_or_else(|| id_of(&shared["itemid"]));
            let file = MediaFile {
                bytes: sizes.into_iter().find_map(Value::as_u64).unwrap_or(0),
                ..MediaFile::new(format!("file:{id}:{}", escaped(&key)), MediaKind::File)
            };
            media.push((file.clone(), url.to_owned()));
            message.attachments.push(Attachment::new(named, file));
            // CONTRACT: an `Attachment` has nowhere to say "open this in the browser", and a
            // SharePoint file answers only to a token for SharePoint, which the media cache is
            // not given. So the file is listed, and its name is also a link that opens it.
            if !parsed.text.is_empty() {
                parsed.text.push('\n');
            }
            let start = parsed.text.len() as u32;
            parsed.text.push_str(named);
            parsed.spans.push(Span { start, end: parsed.text.len() as u32, style: Style::Link { url: url.to_owned() } });
        }
        if let Err(err) = self.ctx.media(&media) {
            eprintln!("chatd: Teams: {err}");
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
        } else if parsed.card || !cards.is_empty() || kind == "RichText/Media_Card" {
            let title = cards.iter().find_map(|card| card_title(card, 6));
            if message.text.is_empty() {
                message.text = title.map_or_else(|| "Card".to_owned(), |title| format!("Card: {}", title.chars().take(200).collect::<String>()));
            }
            message.kind = MessageKind::Unsupported { link: self.message_link(conversation, &id) };
        } else if message.text.is_empty() && message.attachments.is_empty() {
            message.text = "Unsupported message".to_owned();
            message.kind = MessageKind::Unsupported { link: self.message_link(conversation, &id) };
        }
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

    fn arrived(&self, place: &str, root: Option<&str>, message: Message) -> Result<(), Fault> {
        if !self.known(place)? {
            return Ok(());
        }
        // A chat the user hid comes back with a new message, as it does in Teams.
        if self.ctx.known(place)?.is_some_and(|row| row.hidden) {
            let filing = crate::store::Filing { hidden: false, ..crate::store::Filing::default() };
            self.ctx.filing(place, &filing)?;
        }
        let Some(root) = root.filter(|root| *root != message.id) else {
            self.ctx.message(place, &message)?;
            return Ok(());
        };
        // A reply lives in its chain, not in the channel's own pages.
        let chain = chain_id(place, root);
        self.chain_known(place, root)?;
        if message.mentions_me || message.outgoing {
            self.ctx.message(&chain, &message)?;
        } else {
            // Teams tells nobody of a reply unless it names them or they follow the post, and
            // does not say who follows. Opening the chain shows it.
            self.ctx.history(&chain, std::slice::from_ref(&message))?;
            self.ctx.typing_stopped(&chain, &message.sender.id);
        }
        self.bump(place, root, &[&message])
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
    fn bump(&self, channel: &str, root: &str, replies: &[&Message]) -> Result<(), Fault> {
        let Some(mut first) = self.ctx.stored_message(channel, root)? else {
            return Ok(());
        };
        let after = self.summary(channel, root, first.thread.clone(), replies);
        if after != first.thread {
            first.thread = after;
            self.ctx.history(channel, &[first])?;
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
    /// chain, a chat and anything else holds every message itself.
    fn record(&self, conversation: &str, raws: &[Value]) -> Result<(), Fault> {
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
        if let Some((parent, root)) = chain_of(conversation) {
            // The chain whole: the post's summary is exactly what is here.
            let answers: Vec<&Message> = own.iter().filter(|message| message.id != root).collect();
            if let Some(mut first) = self.ctx.stored_message(parent, root)?.filter(|_| answers.len() < CHAIN as usize - 1) {
                let exact = self.summary(parent, root, None, &answers);
                if exact != first.thread {
                    first.thread = exact;
                    self.ctx.history(parent, &[first])?;
                }
            }
        } else if channel {
            for post in &mut own {
                let before = self.ctx.stored_message(conversation, &post.id)?.and_then(|stored| stored.thread);
                post.thread = self.summary(conversation, &post.id, before, &of(&post.id));
            }
        }
        self.ctx.history(conversation, &own)?;
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
                self.bump(conversation, root, &of(root))?;
            }
        }
        Ok(())
    }

    /// The newest page of a conversation; a reply chain whole, from its post on.
    pub(super) fn backfill(&self, conversation: &str) -> Result<(), Fault> {
        if !self.known(conversation)? {
            return Ok(());
        }
        if let Some((channel, root)) = chain_of(conversation) {
            self.chain_known(channel, root)?;
            let (messages, _) = self.page(conversation, CHAIN, "startTime=1")?;
            return self.record(conversation, &messages);
        }
        let (messages, older) = self.page(conversation, PAGE, "startTime=1")?;
        self.remember_cursor(conversation, &messages, &older);
        self.record(conversation, &messages)?;
        self.seen_by(conversation)
    }

    /// Where the page before the oldest message of this one starts, for when it is asked for.
    fn remember_cursor(&self, conversation: &str, messages: &[Value], older: &str) {
        let oldest = messages.iter().map(|raw| id_of(&raw["id"])).filter(|id| millis(id) > 0).min_by_key(|id| millis(id));
        if let Some(oldest) = oldest.filter(|_| !older.is_empty()) {
            self.ctx.set_note(&format!("older:{conversation}"), &format!("{oldest} {older}"));
        }
    }

    /// The page before `before`: by the cursor Teams gave with the page that ended there, or
    /// by time when another page was read since.
    pub(super) fn older(&self, conversation: &str, before: &str) -> Result<(), Fault> {
        if chain_of(conversation).is_some() || millis(before) == 0 {
            return Ok(());
        }
        let note = self.ctx.note(&format!("older:{conversation}")).unwrap_or_default();
        let window = match note.split_once(' ') {
            Some((oldest, cursor)) if oldest == before => format!("startTime=1&syncState={cursor}"),
            _ => format!("startTime=1&endTime={}", millis(before) - 1),
        };
        let (messages, older) = self.page(conversation, PAGE, &window)?;
        self.remember_cursor(conversation, &messages, &older);
        self.record(conversation, &messages)
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
        self.record(conversation, &messages)
    }

    /// The page that ends just after `around`; what follows it is asked for as it is read.
    pub(super) fn around(&self, conversation: &str, around: &str) -> Result<(), Fault> {
        if chain_of(conversation).is_some() || millis(around) == 0 {
            return Ok(());
        }
        let (messages, _) = self.page(conversation, PAGE, &format!("startTime=1&endTime={}", millis(around).saturating_add(1)))?;
        self.record(conversation, &messages)
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
        let outcome = self.posted(conversation, client_id, text, reply_to).map_err(super::reason);
        self.ctx.sent(conversation, client_id, outcome);
    }

    fn posted(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>) -> Result<Message, Fault> {
        self.posted_with(conversation, client_id, text, reply_to, None)
    }

    /// The same, with `properties` (the files a message names) beside the body.
    pub(super) fn posted_with(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, properties: Option<Value>) -> Result<Message, Fault> {
        let mut content = html::compose(text);
        let mut target = conversation.to_owned();
        let quote = match reply_to {
            Some(original) => self.quoted(conversation, original)?,
            None => None,
        };
        let channel = chain_of(conversation).is_none() && self.ctx.known(conversation)?.is_some_and(|row| row.kind == ConversationKind::Channel);
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
        let answer = self.chat("POST", &path, Body::Json(&body), false)?;
        let id = id_of(&answer.body["OriginalArrivalTime"]);
        if millis(&id) == 0 {
            return Err(Fault::Net("Teams accepted the message but did not name it".to_owned()));
        }
        let me = self.me();
        let name = self.own_name();
        let sent = millis(&id) / 1000;
        let mut message = Message::plain(id, name.clone(), text, sent, true);
        message.sender.avatar = self.picture(&me, &name);
        message.sender.id = me;
        if let Some((channel, root)) = chain_of(&target) {
            if target != conversation {
                self.chain_known(channel, root)?;
                self.ctx.history(&target, std::slice::from_ref(&message))?;
            }
            self.bump(channel, root, &[&message])?;
        }
        message.reply_to = quote;
        Ok(message)
    }

    fn message_path(&self, conversation: &str, id: &str) -> String {
        format!("/v1/users/ME/conversations/{}/messages/{}", escaped(conversation), escaped(id))
    }

    pub(super) fn edit(&self, conversation: &str, id: &str, text: &str) -> Result<(), Fault> {
        let stored = self.ctx.stored_message(conversation, id)?;
        let mut content = html::compose(text);
        // The quote above a reply is part of the body, so it is written again.
        if let Some(quote) = stored.as_ref().and_then(|stored| stored.reply_to.as_ref()).filter(|_| chain_of(conversation).is_none()) {
            content = format!("{}{content}", quote_markup(quote));
        }
        let body = self.body(&now_ms().to_string(), &content);
        self.chat("PUT", &self.message_path(conversation, id), Body::Json(&body), false)?;
        if let Some(mut edited) = stored {
            edited.text = text.to_owned();
            edited.spans.clear();
            edited.edited = true;
            self.ctx.history(conversation, &[edited])?;
        }
        Ok(())
    }

    pub(super) fn delete(&self, conversation: &str, id: &str) -> Result<(), Fault> {
        let path = format!("{}?behavior=softDelete", self.message_path(conversation, id));
        self.chat("DELETE", &path, Body::None, false)?;
        self.ctx.deleted(conversation, id)?;
        // A post deleted in its channel is gone from the top of its chain too.
        if chain_of(conversation).is_none() {
            self.ctx.deleted(&chain_id(conversation, id), id)?;
        }
        Ok(())
    }

    pub(super) fn react(&self, conversation: &str, id: &str, emoji: &str, on: bool) -> Result<(), Fault> {
        let learned = || self.ctx.note(&format!("r:{}", bare_emoji(emoji)));
        let Some(key) = key_of(emoji).map(str::to_owned).or_else(learned) else {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaction_keys_read_as_emoji_and_back() {
        assert_eq!((emoji_of("like"), emoji_of("heart"), emoji_of("sad")), ("👍".to_owned(), "❤️".to_owned(), "🙁".to_owned()));
        assert_eq!(emoji_of("1f389_partypopper"), "🎉");
        assert_eq!(emoji_of("1f9d1-200d-1f4bb_technologist"), "🧑\u{200d}💻");
        assert_eq!(emoji_of("like-tone3"), "👍🏽");
        assert_eq!(emoji_of("oreoyum"), ":oreoyum:");
        assert_eq!((key_of("👍"), key_of("👍🏽"), key_of("❤"), key_of("🎉")), (Some("like"), Some("like"), Some("heart"), Some("1f389_partypopper")));
        assert_eq!(key_of("🦑"), None);
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
}
