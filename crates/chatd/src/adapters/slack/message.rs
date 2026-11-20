//! One Slack message, from the socket or from a page of history, as the one message model.

use super::emoji;
use super::text::{emoji_end, Custom, Names, Rich};
use super::{fnv, seconds, thread_id, Slack};
use protocol::{
    Attachment, Card, Forwarded, MediaFile, MediaKind, Message, MessageKind, Preview, Reaction,
    Sender, Span, Style, ThreadSummary, CARD_FIELDS, THREAD_FACES, WAVEFORM_LIMIT,
};
use serde_json::{json, Value};

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

/// What a stored custom-emoji note stands for: `=glyph` is an alias of a standard emoji.
pub(super) fn custom_picture(name: &str, known: &str) -> Custom {
    if let Some(emoji) = known.strip_prefix('=') {
        return Custom::Unicode(emoji.to_owned());
    }
    Custom::File(MediaFile::new(format!("emoji:{name}:{:x}", fnv(known)), MediaKind::Emoji))
}

/// The picker's name list after a rename: the old name is gone and the new one is in it.
pub(super) fn renamed_names(names: &str, old_name: &str, new_name: &str) -> String {
    let mut parts: Vec<&str> = names.split(',').filter(|name| !name.is_empty() && *name != old_name && *name != new_name).collect();
    if !new_name.is_empty() {
        parts.push(new_name);
    }
    parts.sort_unstable();
    parts.join(",")
}

/// A custom emoji reads back as a picture, and a rename keeps that picture under the new name.
pub(super) fn prove_custom_emoji() -> Result<(), String> {
    struct Book;
    impl Names for Book {
        fn me(&self) -> &str {
            ""
        }
        fn user(&self, id: &str) -> String {
            id.to_owned()
        }
        fn channel(&self, _id: &str) -> Option<String> {
            None
        }
        fn usergroup(&self, _id: &str) -> Option<String> {
            None
        }
        fn custom(&self, name: &str) -> Option<Custom> {
            (name == "partyparrot").then(|| custom_picture(name, "pic"))
        }
    }
    let mut rich = Rich::new(&Book);
    rich.mrkdwn("see :partyparrot:");
    if rich.text != "see :partyparrot:" {
        return Err(format!("a custom emoji was written as {}", rich.text));
    }
    let Some(span) = rich.spans.iter().find(|span| matches!(span.style, Style::Emoji { .. })) else {
        return Err("a custom emoji was not a picture".to_owned());
    };
    let Style::Emoji { file } = &span.style else {
        return Err("a custom emoji was not a picture".to_owned());
    };
    let Custom::File(want) = custom_picture("partyparrot", "pic") else {
        return Err("a picture note was not a file".to_owned());
    };
    if file.id != want.id || file.kind != MediaKind::Emoji {
        return Err(format!("the emoji file was {}", file.id));
    }
    let names = renamed_names("parrot,partyparrot,shipit", "partyparrot", "party-parrot");
    if names.split(',').any(|name| name == "partyparrot") || !names.split(',').any(|name| name == "party-parrot") {
        return Err(format!("a rename left {names}"));
    }
    Ok(())
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

/// An old Slack call (`sh_room_created`) uses the same room clock as a huddle.
fn legacy_call(room: &Value) -> String {
    let (start, end) = (room["date_start"].as_i64().unwrap_or(0), room["date_end"].as_i64().unwrap_or(0));
    if room["has_ended"] == true || end > 0 {
        match (end - start) / 60 {
            minutes if start > 0 && minutes > 0 => format!("A call happened · {minutes} min"),
            _ => "A call happened".to_owned(),
        }
    } else {
        "A call started".to_owned()
    }
}

/// The app a `call` block names. Slack's delivered block puts `media_backend_type` on `call`
/// or on `call.v1` (Calls API). `app_name` is used when the block already spells it.
fn meeting_app(block: &Value) -> String {
    let v1 = &block["call"]["v1"];
    let raw = first(&[&v1["app_name"], &v1["media_backend_type"], &block["call"]["media_backend_type"]]).unwrap_or("");
    match raw.trim().to_ascii_lowercase().as_str() {
        "zoom" => "Zoom".to_owned(),
        "google_meet" | "google-meet" | "meet" => "Google Meet".to_owned(),
        "microsoft_teams" | "microsoft-teams" | "teams" | "ms_teams" => "Microsoft Teams".to_owned(),
        "webex" | "cisco_webex" | "cisco-webex" => "Webex".to_owned(),
        "" => "Meeting".to_owned(),
        other => other.to_owned(),
    }
}

/// "Zoom meeting · 3 people", or "Ended · 25 min" once `date_end` is set.
fn meeting_line(app: &str, v1: &Value) -> String {
    let start = v1["date_start"].as_i64().unwrap_or(0);
    let end = v1["date_end"].as_i64().unwrap_or(0);
    if v1["has_ended"] == true || end > 0 {
        let minutes = if start > 0 && end > start { (end - start) / 60 } else { 0 };
        return if minutes > 0 { format!("Ended · {minutes} min") } else { "Ended".to_owned() };
    }
    let people = v1["active_participants"].as_array().map(Vec::len).unwrap_or(0);
    let kind = if app == "Meeting" { "Meeting".to_owned() } else { format!("{app} meeting") };
    match people {
        0 => kind,
        1 => format!("{kind} · 1 person"),
        count => format!("{kind} · {count} people"),
    }
}

/// A Join link for `call.v1.join_url`. Only a web address: the meeting app opens that.
fn join_link(app: &str, v1: &Value) -> Option<Preview> {
    let url = v1["join_url"].as_str()?;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    Some(Preview {
        url: url.to_owned(),
        site: app.to_owned(),
        title: "Join".to_owned(),
        description: String::new(),
        image: None,
    })
}

/// The first `call` block that Slack has filled in (`call.v1`). None when the block is only
/// a `call_id` with nothing to show.
fn meeting_from(blocks: &[Value]) -> Option<(Card, Option<Preview>)> {
    let block = blocks.iter().find(|block| block["type"] == "call")?;
    let v1 = &block["call"]["v1"];
    if !v1.is_object() {
        return None;
    }
    let app = meeting_app(block);
    let card = Card {
        title: first(&[&v1["name"]]).unwrap_or("Call").to_owned(),
        text: meeting_line(&app, v1),
        ..Card::default()
    };
    Some((card, join_link(&app, v1)))
}

/// What the slack-call-cards driver checks, with no workspace and no socket.
pub(super) fn prove_call_cards() -> Result<(), String> {
    let started = 1_700_000_000_i64;
    let zoom = json!({
        "blocks": [{
            "type": "call", "call_id": "R0ZOOM",
            "call": {
                "media_backend_type": "zoom",
                "v1": {
                    "name": "Standup", "join_url": "https://zoom.us/j/123456789", "date_start": started,
                    "active_participants": [{}, {}, {}],
                },
            },
        }],
    });
    let (card, join) = meeting_from(zoom["blocks"].as_array().unwrap()).ok_or("a Zoom call was not a card")?;
    if card.title != "Standup" || card.text != "Zoom meeting · 3 people" {
        return Err(format!("Zoom card was {} / {}", card.title, card.text));
    }
    let join = join.ok_or("a Zoom call had no Join link")?;
    if join.title != "Join" || join.url != "https://zoom.us/j/123456789" {
        return Err(format!("Join was {} {}", join.title, join.url));
    }
    let ended = json!({
        "blocks": [{
            "type": "call",
            "call": { "media_backend_type": "google_meet", "v1": { "name": "Planning", "date_start": started, "date_end": started + 25 * 60 } },
        }],
    });
    let (card, _) = meeting_from(ended["blocks"].as_array().unwrap()).ok_or("an ended call was not a card")?;
    if card.text != "Ended · 25 min" {
        return Err(format!("ended call said {}", card.text));
    }
    let teams = json!({
        "blocks": [{
            "type": "call",
            "call": { "v1": { "media_backend_type": "microsoft_teams", "name": "Review", "date_start": started, "active_participants": [{}] } },
        }],
    });
    let (card, _) = meeting_from(teams["blocks"].as_array().unwrap()).ok_or("a Teams call was not a card")?;
    if card.text != "Microsoft Teams meeting · 1 person" {
        return Err(format!("Teams call said {}", card.text));
    }
    let webex = json!({
        "blocks": [{ "type": "call", "call": { "v1": { "media_backend_type": "webex", "name": "Sync", "date_start": started } } }],
    });
    let (card, _) = meeting_from(webex["blocks"].as_array().unwrap()).ok_or("a Webex call was not a card")?;
    if card.text != "Webex meeting" {
        return Err(format!("Webex call said {}", card.text));
    }
    if meeting_from(&[json!({"type": "call", "call_id": "R0"})]).is_some() {
        return Err("a call id with no v1 invented a card".to_owned());
    }
    let room = json!({"date_start": started, "date_end": started + 25 * 60, "has_ended": true});
    let line = legacy_call(&room);
    if line != "A call happened · 25 min" {
        return Err(format!("an old call said {line}"));
    }
    if huddle(&room) != "A huddle happened · 25 min" {
        return Err("a huddle was renamed".to_owned());
    }
    Ok(())
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

    /// A custom emoji as the workspace's list names it. The first message that names one with no
    /// note asks for the list (`emoji.list`), once a day, the same way the picker does.
    fn custom(&self, name: &str) -> Option<Custom> {
        if name.is_empty() {
            return None;
        }
        let note = format!("e:{name}");
        match self.ctx.note(&note) {
            Some(known) if known.is_empty() => None,
            Some(known) => self.custom_known(name, &known),
            None => {
                self.list_emoji();
                let known = self.ctx.note(&note)?;
                if known.is_empty() { None } else { self.custom_known(name, &known) }
            }
        }
    }
}

impl Slack {
    fn custom_known(&self, name: &str, known: &str) -> Option<Custom> {
        match custom_picture(name, known) {
            Custom::Unicode(emoji) => Some(Custom::Unicode(emoji)),
            Custom::File(file) => {
                if let Err(err) = self.ctx.media(&[(file.clone(), known.to_owned())]) {
                    tracing::warn!("chatd: Slack: {err}");
                }
                Some(Custom::File(file))
            }
        }
    }

    /// The workspace's custom emoji, written to the store once a day at most. The picker asks,
    /// and so does the first message that names one the store has no note for. Nothing of the
    /// list stays in memory. True when this call fetched it.
    pub(super) fn list_emoji(&self) -> bool {
        let now = crate::hub::now();
        let listed = self.ctx.note("emoji").and_then(|at| at.parse::<i64>().ok());
        if listed.is_some_and(|at| now.saturating_sub(at) < 86_400) {
            return false;
        }
        self.ctx.set_note("emoji", &now.to_string());
        let Ok(list) = self.api("emoji.list", &[]) else {
            return false;
        };
        let Some(all) = list["emoji"].as_object() else {
            return false;
        };
        // The names, for the picker: one note, not one read per emoji.
        let mut names: Vec<&str> = all.keys().map(String::as_str).collect();
        names.sort_unstable();
        self.ctx.set_note("emoji:names", &names.join(","));
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
        self.repaint_emoji();
        true
    }

    /// One custom emoji changed: `emoji_changed` on the socket.
    pub(super) fn emoji_changed(&self, event: &Value) {
        match event["subtype"].as_str() {
            Some("add") => {
                let (name, value) = (event["name"].as_str().unwrap_or(""), event["value"].as_str().unwrap_or(""));
                if !name.is_empty() && self.hosted(value) {
                    self.ctx.set_note(&format!("e:{name}"), value);
                    let names = self.ctx.note("emoji:names").unwrap_or_default();
                    if !names.split(',').any(|known| known == name) {
                        self.ctx.set_note("emoji:names", &format!("{names},{name}"));
                    }
                    self.repaint_emoji();
                }
            }
            Some("remove") => {
                for name in event["names"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    self.ctx.set_note(&format!("e:{name}"), "");
                }
                self.repaint_emoji();
            }
            // The new name keeps the picture now. Waiting a day would fetch the old list again.
            Some("rename") => {
                let old_name = event["old_name"].as_str().unwrap_or("");
                let new_name = event["new_name"].as_str().unwrap_or("");
                if old_name.is_empty() || new_name.is_empty() {
                    return;
                }
                let value = event["value"].as_str().unwrap_or("");
                let picture = if self.hosted(value) {
                    value.to_owned()
                } else if let Some(alias) = value.strip_prefix("alias:") {
                    match emoji::standard(alias) {
                        Some(glyph) => format!("={glyph}"),
                        None => self.ctx.note(&format!("e:{alias}")).filter(|known| !known.is_empty()).unwrap_or_default(),
                    }
                } else {
                    self.ctx.note(&format!("e:{old_name}")).unwrap_or_default()
                };
                if !picture.is_empty() {
                    self.ctx.set_note(&format!("e:{new_name}"), &picture);
                }
                self.ctx.set_note(&format!("e:{old_name}"), "");
                let names = self.ctx.note("emoji:names").unwrap_or_default();
                self.ctx.set_note("emoji:names", &renamed_names(&names, old_name, new_name));
                self.repaint_emoji();
            }
            _ => self.ctx.set_note("emoji", "0"),
        }
    }

    /// A status emoji: a standard one as itself, a custom one as `:name:` with its picture
    /// registered the way a reaction's is. The live emoji is the short name, which fits where
    /// a file id does not.
    pub(super) fn status_emoji(&self, raw: &str) -> String {
        let name = raw.trim().trim_matches(':');
        if name.is_empty() {
            return String::new();
        }
        if let Some(glyph) = emoji::standard(name) {
            return glyph.to_owned();
        }
        match self.custom(name) {
            Some(Custom::Unicode(glyph)) => glyph,
            Some(Custom::File(_)) | None => format!(":{name}:"),
        }
    }

    /// Rows already on screen were stored before this name had a picture. Fill those in place.
    /// A page is not fetched again: the tests count `conversations.history`.
    fn repaint_emoji(&self) {
        let watching = self.ctx.watching();
        let Some(open) = watching.conversation.as_ref().filter(|open| open.account() == self.ctx.account) else {
            return;
        };
        // The newest page, which is what is on screen. `stored_ids` orders by id length first,
        // so a short upper bound would miss every Slack timestamp.
        let loaded = {
            let mut store = self.ctx.hub.store();
            store.messages(&self.ctx.key(&open.id), &protocol::Page::Newest, 50, crate::hub::now())
        };
        let Ok(Some(page)) = loaded else {
            return;
        };
        let mut changed = Vec::new();
        for mut message in page.messages {
            if self.paint_emoji(&mut message) {
                changed.push(message);
            }
        }
        if !changed.is_empty() {
            let _ = self.ctx.history(&open.id, &changed);
        }
    }

    /// Adds a picture to a custom emoji this message still shows as a bare `:name:`.
    fn paint_emoji(&self, message: &mut Message) -> bool {
        let mut changed = false;
        for reaction in &mut message.reactions {
            if reaction.file.is_some() {
                continue;
            }
            let Some(name) = reaction.emoji.strip_prefix(':').and_then(|rest| rest.strip_suffix(':')) else {
                continue;
            };
            if name.is_empty() || emoji::standard(name).is_some() {
                continue;
            }
            match self.custom(name) {
                Some(Custom::File(file)) => {
                    reaction.file = Some(file);
                    changed = true;
                }
                Some(Custom::Unicode(glyph)) => {
                    reaction.emoji = glyph;
                    changed = true;
                }
                None => {}
            }
        }
        let text = message.text.clone();
        let bytes = text.as_bytes();
        let mut at = 0;
        let mut extra = Vec::new();
        while at < bytes.len() {
            if bytes[at] != b':' {
                at += 1;
                continue;
            }
            let Some(close) = emoji_end(&text, at) else {
                at += 1;
                continue;
            };
            let end = close + 1;
            let name = &text[at + 1..close];
            let (start, end) = (at as u32, end as u32);
            let covered = message.spans.iter().any(|span| span.start < end && start < span.end);
            if !covered && !name.is_empty() && emoji::standard(name).is_none() {
                if let Some(Custom::File(file)) = self.custom(name) {
                    extra.push(Span { start, end, style: Style::Emoji { file } });
                }
            }
            at = end as usize;
        }
        if !extra.is_empty() {
            message.spans.extend(extra);
            changed = true;
        }
        changed
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
        // Beside a GIF's stills Slack makes moving copies at 360 and 480 (`thumb_480_gif`, the
        // still's size): those play in the row for a fraction of the original.
        let moving: Vec<_> = [360, 480]
            .iter()
            .filter(|_| kind == MediaKind::Gif)
            .filter_map(|side| {
                let (url, ..) = still(&format!("thumb_{side}_gif"), *side)?;
                let (_, width, height, longest) = still(&format!("thumb_{side}"), *side).unwrap_or((String::new(), 0, 0, *side));
                Some((url, width, height, longest))
            })
            .collect();
        let mut sizes = match kind {
            MediaKind::Gif if !moving.is_empty() => moving,
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
            preview: None,
            mime: mime.to_owned(),
            duration: (file["duration_ms"].as_u64().unwrap_or(0) / 1000) as u32,
            round: false,
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
            sponsored: false,
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
            // Only a domain already looked up for a Slack Connect channel. A forward is not
            // worth a lookup, and Slack does not send the workspace's name.
            from_space: first(&[&card["from_url"]]).and_then(|url| self.workspace_named(url)).unwrap_or_default(),
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
            // `sh_room_created` is an old Slack call. A huddle uses the same room object.
            rich.text = if subtype == Some("sh_room_created") { legacy_call(&event["room"]) } else { huddle(&event["room"]) };
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
        if framed.is_none() {
            if let Some(blocks) = event["blocks"].as_array() {
                if let Some((mut card, join)) = meeting_from(blocks) {
                    card.title = self.words(&card.title);
                    framed = Some(Box::new(card));
                    previews.extend(join);
                }
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
            let what = if rich.undrawn { "An interactive message" } else { "A message tinyclient cannot show yet" };
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
            tracing::warn!("chatd: Slack: {err}");
        }
        Some(message)
    }
}
