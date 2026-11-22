//! One Slack message, from the socket or from a page of history, as the one message model.

use super::emoji;
use super::text::{emoji_end, Custom, Names, Rich};
use super::{fnv, seconds, thread_id, Slack};
use protocol::{
    Attachment, Card, ConfirmDialog, ControlKind, ControlOption, ControlStyle, Forwarded,
    MediaFile, MediaKind, Message, MessageControl, MessageKind, Preview, Reaction, Sender, Span,
    Style, ThreadSummary, Transcript, CARD_FIELDS, CONTROL_OPTIONS_LIMIT, MESSAGE_CONTROLS_LIMIT,
    THREAD_FACES, WAVEFORM_LIMIT,
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

/// True when a Slack file entry represents a Canvas (`quip`, `canvas`, `slack_canvas`).
pub(super) fn is_canvas(file: &Value) -> bool {
    if file["is_canvas"] == true {
        return true;
    }
    let ft = file["filetype"].as_str().unwrap_or("").to_ascii_lowercase();
    if matches!(ft.as_str(), "quip" | "canvas" | "slack_canvas") {
        return true;
    }
    let mime = file["mimetype"].as_str().unwrap_or("").to_ascii_lowercase();
    matches!(mime.as_str(), "application/vnd.slack-docs" | "application/vnd.slack.canvas")
}

/// True when a Slack file entry represents a Slack List (`list`, `slack_list`).
pub(super) fn is_list(file: &Value) -> bool {
    if file["is_list"] == true {
        return true;
    }
    let ft = file["filetype"].as_str().unwrap_or("").to_ascii_lowercase();
    if matches!(ft.as_str(), "list" | "slack_list") {
        return true;
    }
    let mime = file["mimetype"].as_str().unwrap_or("").to_ascii_lowercase();
    matches!(mime.as_str(), "application/vnd.slack-list" | "application/vnd.slack.list")
}

/// True when a Slack file entry is an inline code/text snippet (`mode == "snippet"`).
pub(super) fn is_snippet(file: &Value) -> bool {
    file["mode"].as_str() == Some("snippet")
}

/// True when a Slack file entry is a legacy Slack Post (`mode == "post"` or `filetype` `post`/`space`).
pub(super) fn is_post(file: &Value) -> bool {
    if file["mode"].as_str() == Some("post") {
        return true;
    }
    let ft = file["filetype"].as_str().unwrap_or("").to_ascii_lowercase();
    matches!(ft.as_str(), "post" | "space")
}

/// Human-readable provider name for an externally hosted file (Google Drive, Dropbox, OneDrive, Box).
pub(super) fn external_provider(file: &Value) -> String {
    let raw = file["external_type"].as_str().unwrap_or("").trim().to_ascii_lowercase();
    match raw.as_str() {
        "gdrive" | "google_drive" | "google" => return "Google Drive".to_owned(),
        "dropbox" => return "Dropbox".to_owned(),
        "onedrive" | "sharepoint" => return "OneDrive".to_owned(),
        "box" => return "Box".to_owned(),
        _ => {}
    }
    let url = first(&[&file["url_private"], &file["permalink"]]).unwrap_or("").to_ascii_lowercase();
    if url.contains("docs.google.com") || url.contains("drive.google.com") {
        "Google Drive".to_owned()
    } else if url.contains("dropbox.com") {
        "Dropbox".to_owned()
    } else if url.contains("onedrive.live.com") || url.contains("sharepoint.com") {
        "OneDrive".to_owned()
    } else if url.contains("box.com") {
        "Box".to_owned()
    } else if !raw.is_empty() {
        raw
    } else {
        "External file".to_owned()
    }
}

/// Appends a Slack snippet's inline preview as a `Style::CodeBlock` span onto `rich`.
fn append_snippet_preview(file: &Value, rich: &mut Rich) {
    let raw = first(&[&file["preview"], &file["plain_text"], &file["preview_plain_text"]]);
    let Some(code) = raw else {
        return;
    };
    let title = first(&[&file["title"], &file["name"]]).unwrap_or("");
    if !title.is_empty() {
        rich.line();
        let start = rich.text.len() as u32;
        rich.text.push_str(title);
        let end = rich.text.len() as u32;
        if end > start {
            rich.spans.push(Span { start, end, style: Style::Bold });
        }
    }
    rich.line();
    let start = rich.text.len() as u32;
    rich.text.push_str(code);
    let end = rich.text.len() as u32;
    if end > start {
        let language = file["filetype"].as_str().unwrap_or("").to_owned();
        rich.spans.push(Span { start, end, style: Style::CodeBlock { language } });
    }
    let more = file["lines_more"].as_u64().unwrap_or(0);
    if more > 0 {
        rich.line();
        let total = file["lines"].as_u64().unwrap_or(0);
        if total > 0 {
            rich.text.push_str(&format!("… {more} more lines ({total} total)"));
        } else {
            rich.text.push_str(&format!("… {more} more lines"));
        }
    }
}

/// Decodes common HTML entities and strips HTML markup into clean, readable plain text.
pub(super) fn strip_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    let mut skip_until: Option<&str> = None;
    while !rest.is_empty() {
        if let Some(end_tag) = skip_until {
            if let Some(pos) = rest.to_ascii_lowercase().find(end_tag) {
                rest = &rest[pos + end_tag.len()..];
            } else {
                break;
            }
            skip_until = None;
            continue;
        }
        if let Some(lt) = rest.find('<') {
            push_entities(&mut out, &rest[..lt]);
            let after = &rest[lt + 1..];
            let Some(gt) = after.find('>') else {
                // Unclosed angle bracket: keep text without raw partial tag if it looks like a tag.
                break;
            };
            let tag_raw = after[..gt].trim();
            let tag_lower = tag_raw.to_ascii_lowercase();
            let name = tag_lower
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("");
            if !tag_lower.starts_with('/') && matches!(name, "script" | "style") {
                skip_until = Some(if name == "script" { "</script>" } else { "</style>" });
            } else if name == "input" && tag_lower.contains("checkbox") {
                if tag_lower.contains("checked") {
                    out.push_str("☑ ");
                } else {
                    out.push_str("☐ ");
                }
            } else if name == "li" && !tag_lower.starts_with('/') {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("• ");
            } else if matches!(name, "br" | "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "section" | "hr") {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            rest = &after[gt + 1..];
        } else {
            push_entities(&mut out, rest);
            break;
        }
    }
    // Collapse excess blank lines and trim each line.
    let mut lines: Vec<String> = Vec::new();
    let mut prev_blank = false;
    for line in out.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_blank && !lines.is_empty() {
                lines.push(String::new());
                prev_blank = true;
            }
        } else {
            lines.push(trimmed.to_owned());
            prev_blank = false;
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

fn push_entities(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';').filter(|&i| i > 0 && i <= 10) {
            let entity = &after[..semi];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" | "#x27" | "#X27" => Some('\''),
                "nbsp" => Some(' '),
                s if s.starts_with("#x") || s.starts_with("#X") => {
                    u32::from_str_radix(&s[2..], 16).ok().and_then(char::from_u32)
                }
                s if s.starts_with('#') => s[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
}

fn push_card(framed: &mut Option<Box<Card>>, extra: &mut Vec<Card>, card: Card) {
    if framed.is_none() {
        *framed = Some(Box::new(card));
    } else {
        extra.push(card);
    }
}

fn parse_confirm(raw: &Value) -> Option<ConfirmDialog> {
    if !raw.is_object() {
        return None;
    }
    let title = raw["title"]["text"].as_str().or_else(|| raw["title"].as_str()).unwrap_or("").trim().to_owned();
    let text = raw["text"]["text"].as_str().or_else(|| raw["text"].as_str()).unwrap_or("").trim().to_owned();
    let confirm = raw["confirm"]["text"]
        .as_str()
        .or_else(|| raw["confirm"].as_str())
        .or_else(|| raw["ok_text"].as_str())
        .unwrap_or("Confirm")
        .trim()
        .to_owned();
    let deny = raw["deny"]["text"]
        .as_str()
        .or_else(|| raw["deny"].as_str())
        .or_else(|| raw["dismiss_text"].as_str())
        .unwrap_or("Cancel")
        .trim()
        .to_owned();
    if title.is_empty() && text.is_empty() {
        return None;
    }
    Some(ConfirmDialog { title, text, confirm, deny })
}

fn parse_options(el: &Value) -> Vec<ControlOption> {
    let mut out = Vec::new();
    let push_opt = |opt: &Value, out: &mut Vec<ControlOption>| {
        if out.len() >= CONTROL_OPTIONS_LIMIT {
            return;
        }
        let label = opt["text"]["text"]
            .as_str()
            .or_else(|| opt["text"].as_str())
            .or_else(|| opt["label"].as_str())
            .unwrap_or("")
            .trim();
        let value = opt["value"].as_str().unwrap_or(label).trim();
        if !label.is_empty() || !value.is_empty() {
            out.push(ControlOption {
                label: if label.is_empty() { value.to_owned() } else { label.to_owned() },
                value: value.to_owned(),
            });
        }
    };
    for opt in el["options"].as_array().into_iter().flatten() {
        push_opt(opt, &mut out);
    }
    for group in el["option_groups"].as_array().into_iter().flatten() {
        for opt in group["options"].as_array().into_iter().flatten() {
            push_opt(opt, &mut out);
        }
    }
    out
}

fn parse_button_style(raw: Option<&str>) -> ControlStyle {
    match raw {
        Some("primary") => ControlStyle::Primary,
        Some("danger") => ControlStyle::Danger,
        _ => ControlStyle::Default,
    }
}

fn parse_element(block_id: &str, el: &Value, label_override: Option<&str>, fallback_idx: usize) -> Option<MessageControl> {
    let kind = el["type"].as_str().filter(|s| !s.is_empty())?;
    let action_id = el["action_id"]
        .as_str()
        .or_else(|| el["name"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{kind}_{fallback_idx}"));
    let confirm = parse_confirm(&el["confirm"]);
    match kind {
        "button" => {
            let label = el["text"]["text"]
                .as_str()
                .or_else(|| el["text"].as_str())
                .or(label_override)
                .unwrap_or("Action")
                .trim()
                .to_owned();
            let url = el["url"]
                .as_str()
                .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
                .unwrap_or("")
                .to_owned();
            let value = el["value"].as_str().unwrap_or("").to_owned();
            let style = parse_button_style(el["style"].as_str());
            Some(MessageControl {
                block_id: block_id.to_owned(),
                action_id,
                element_type: kind.to_owned(),
                label,
                value,
                confirm,
                kind: ControlKind::Button { url, style },
            })
        }
        "static_select"
        | "external_select"
        | "users_select"
        | "conversations_select"
        | "channels_select"
        | "multi_static_select"
        | "multi_external_select"
        | "multi_users_select"
        | "multi_conversations_select"
        | "multi_channels_select" => {
            let placeholder = el["placeholder"]["text"]
                .as_str()
                .or_else(|| el["placeholder"].as_str())
                .or(label_override)
                .unwrap_or("Select an option")
                .trim()
                .to_owned();
            let options = parse_options(el);
            let selected = el["initial_option"]["value"]
                .as_str()
                .or_else(|| el["initial_user"].as_str())
                .or_else(|| el["initial_conversation"].as_str())
                .or_else(|| el["initial_channel"].as_str())
                .map(str::to_owned);
            let external = matches!(
                kind,
                "external_select"
                    | "multi_external_select"
                    | "users_select"
                    | "conversations_select"
                    | "channels_select"
                    | "multi_users_select"
                    | "multi_conversations_select"
                    | "multi_channels_select"
            );
            Some(MessageControl {
                block_id: block_id.to_owned(),
                action_id,
                element_type: kind.to_owned(),
                label: label_override.unwrap_or("").trim().to_owned(),
                value: selected.clone().unwrap_or_default(),
                confirm,
                kind: ControlKind::Select { placeholder, options, selected, external },
            })
        }
        "overflow" => {
            let options = parse_options(el);
            if options.is_empty() {
                return None;
            }
            Some(MessageControl {
                block_id: block_id.to_owned(),
                action_id,
                element_type: kind.to_owned(),
                label: label_override.unwrap_or("More…").trim().to_owned(),
                value: String::new(),
                confirm,
                kind: ControlKind::Select {
                    placeholder: "More…".to_owned(),
                    options,
                    selected: None,
                    external: false,
                },
            })
        }
        "datepicker" => {
            let placeholder = el["placeholder"]["text"]
                .as_str()
                .or(label_override)
                .unwrap_or("Select a date")
                .trim()
                .to_owned();
            let initial = el["initial_date"].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
            Some(MessageControl {
                block_id: block_id.to_owned(),
                action_id,
                element_type: kind.to_owned(),
                label: label_override.unwrap_or("").trim().to_owned(),
                value: initial.clone().unwrap_or_default(),
                confirm,
                kind: ControlKind::DatePicker { placeholder, initial },
            })
        }
        "timepicker" => {
            let placeholder = el["placeholder"]["text"]
                .as_str()
                .or(label_override)
                .unwrap_or("Select a time")
                .trim()
                .to_owned();
            let initial = el["initial_time"].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
            Some(MessageControl {
                block_id: block_id.to_owned(),
                action_id,
                element_type: kind.to_owned(),
                label: label_override.unwrap_or("").trim().to_owned(),
                value: initial.clone().unwrap_or_default(),
                confirm,
                kind: ControlKind::TimePicker { placeholder, initial },
            })
        }
        "checkboxes" => {
            let options = parse_options(el);
            if options.is_empty() {
                return None;
            }
            let selected: Vec<String> = el["initial_options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| o["value"].as_str().map(str::to_owned))
                .collect();
            Some(MessageControl {
                block_id: block_id.to_owned(),
                action_id,
                element_type: kind.to_owned(),
                label: label_override.unwrap_or("").trim().to_owned(),
                value: String::new(),
                confirm,
                kind: ControlKind::Checkboxes { options, selected },
            })
        }
        "radio_buttons" => {
            let options = parse_options(el);
            if options.is_empty() {
                return None;
            }
            let selected = el["initial_option"]["value"].as_str().map(str::to_owned);
            Some(MessageControl {
                block_id: block_id.to_owned(),
                action_id,
                element_type: kind.to_owned(),
                label: label_override.unwrap_or("").trim().to_owned(),
                value: selected.clone().unwrap_or_default(),
                confirm,
                kind: ControlKind::RadioButtons { options, selected },
            })
        }
        _ => None,
    }
}

fn collect_block_controls(blocks: &[Value], out: &mut Vec<MessageControl>) {
    for block in blocks {
        if out.len() >= MESSAGE_CONTROLS_LIMIT {
            break;
        }
        let block_id = block["block_id"].as_str().unwrap_or("");
        match block["type"].as_str() {
            Some("actions") => {
                for el in block["elements"].as_array().into_iter().flatten() {
                    if out.len() >= MESSAGE_CONTROLS_LIMIT {
                        break;
                    }
                    if let Some(ctrl) = parse_element(block_id, el, None, out.len()) {
                        out.push(ctrl);
                    }
                }
            }
            Some("section") => {
                let acc = &block["accessory"];
                if acc.is_object() && acc["type"].as_str() != Some("image") {
                    if let Some(ctrl) = parse_element(block_id, acc, None, out.len()) {
                        out.push(ctrl);
                    }
                }
            }
            Some("input") => {
                let label = block["label"]["text"].as_str();
                if let Some(ctrl) = parse_element(block_id, &block["element"], label, out.len()) {
                    out.push(ctrl);
                }
            }
            _ => {}
        }
    }
}

fn parse_controls(event: &Value) -> Vec<MessageControl> {
    let mut out = Vec::new();
    if let Some(blocks) = event["blocks"].as_array() {
        collect_block_controls(blocks, &mut out);
    }
    for att in event["attachments"].as_array().into_iter().flatten() {
        if out.len() >= MESSAGE_CONTROLS_LIMIT {
            break;
        }
        if let Some(blocks) = att["blocks"].as_array() {
            collect_block_controls(blocks, &mut out);
        }
        let callback_id = att["callback_id"].as_str().unwrap_or("");
        for act in att["actions"].as_array().into_iter().flatten() {
            if out.len() >= MESSAGE_CONTROLS_LIMIT {
                break;
            }
            let kind = act["type"].as_str().unwrap_or("");
            if kind == "button" {
                if let Some(ctrl) = parse_element(callback_id, act, None, out.len()) {
                    out.push(ctrl);
                }
            } else if kind == "select" {
                let mut normalized = act.clone();
                normalized["type"] = "static_select".into();
                if let Some(ctrl) = parse_element(callback_id, &normalized, None, out.len()) {
                    out.push(ctrl);
                }
            }
        }
    }
    out.truncate(MESSAGE_CONTROLS_LIMIT);
    out
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
        self.load_usergroups();
        self.ctx.note(&note)
    }

    fn in_usergroup(&self, id: &str) -> bool {
        self.load_usergroups();
        self.state().my_groups.contains(id)
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

    fn hosted(&self, url: &str) -> bool {
        Slack::hosted(self, url)
    }
}

impl Slack {
    /// Reads the workspace's active user groups once per connection (`usergroups.list` with
    /// `include_users=true`), records their handles, and remembers which ones include me.
    /// Disabled groups (`date_delete > 0`) are ignored.
    pub(super) fn load_usergroups(&self) {
        if std::mem::replace(&mut self.state().groups_listed, true) {
            return;
        }
        let Ok(listed) = self.api("usergroups.list", &[("include_users", "true")]) else {
            return;
        };
        let me = self.me.get().map_or("", String::as_str);
        let mut mine = std::collections::HashSet::new();
        for group in listed["usergroups"].as_array().into_iter().flatten() {
            let disabled = group["date_delete"].as_i64().unwrap_or(0) > 0
                || group["deleted_by"].as_str().is_some_and(|by| !by.is_empty());
            if disabled {
                continue;
            }
            let Some(id) = group["id"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            if let Some(handle) = group["handle"].as_str().filter(|handle| !handle.is_empty()) {
                self.ctx.set_note(&format!("g:{id}"), handle);
            }
            let member = !me.is_empty()
                && group["users"]
                    .as_array()
                    .is_some_and(|users| users.iter().any(|user| user.as_str() == Some(me)));
            if member {
                mine.insert(id.to_owned());
            }
        }
        self.state().my_groups = mine;
    }

    /// `subteam_self_added` / `subteam_self_removed`: updates the set of user groups I belong to.
    pub(super) fn subteam_self(&self, event: &Value, added: bool) {
        let Some(id) = event["subteam_id"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let mut state = self.state();
        if added {
            state.my_groups.insert(id.to_owned());
        } else {
            state.my_groups.remove(id);
        }
    }

    /// `subteam_created` / `subteam_updated`: keeps handles current and drops disabled groups.
    pub(super) fn subteam_updated(&self, event: &Value) {
        let subteam = &event["subteam"];
        let Some(id) = subteam["id"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let disabled = subteam["date_delete"].as_i64().unwrap_or(0) > 0
            || subteam["deleted_by"].as_str().is_some_and(|by| !by.is_empty());
        if disabled {
            self.state().my_groups.remove(id);
            return;
        }
        if let Some(handle) = subteam["handle"].as_str().filter(|handle| !handle.is_empty()) {
            self.ctx.set_note(&format!("g:{id}"), handle);
        }
    }

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
    pub(super) fn permalink(&self, channel: &str, ts: &str, thread: Option<&str>) -> String {
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
                ..Sender::default()
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
            ..Sender::default()
        }
    }

    /// True when a Slack file is hosted outside Slack (Google Drive, Dropbox, Box, OneDrive)
    /// and should open externally rather than attempting a Slack blob download.
    pub(super) fn is_external(&self, file: &Value) -> bool {
        if file["mode"].as_str() == Some("external") || file["is_external"] == true {
            return true;
        }
        file["url_private"]
            .as_str()
            .is_some_and(|u| (u.starts_with("https://") || u.starts_with("http://")) && !self.hosted(u))
    }

    /// Maps an externally hosted file (`mode == "external"`, Google Drive, Dropbox, etc.) into a
    /// structured card with an open link instead of attempting a broken Slack blob download.
    fn external_file_card(&self, file: &Value, fallback_link: &str) -> Card {
        let provider = external_provider(file);
        let title = first(&[&file["title"], &file["name"]]).unwrap_or("External file").trim().to_owned();
        let link = first(&[&file["url_private"], &file["permalink"]])
            .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
            .unwrap_or(fallback_link)
            .to_owned();
        let mut parts: Vec<String> = Vec::new();
        if let Some(pretty) = first(&[&file["pretty_type"]]) {
            parts.push(pretty.to_owned());
        }
        let size = file["size"].as_u64().unwrap_or(0);
        if size > 0 {
            let label = if size >= 1024 * 1024 {
                format!("{:.1} MB", size as f64 / (1024. * 1024.))
            } else if size >= 1024 {
                format!("{} KB", size.div_ceil(1024))
            } else {
                format!("{size} B")
            };
            parts.push(label);
        }
        let preview = first(&[&file["preview_plain_text"], &file["preview"]])
            .map(strip_html)
            .unwrap_or_default();
        let text = match (parts.is_empty(), preview.is_empty()) {
            (false, false) => format!("{}\n{}", parts.join(" · "), preview),
            (false, true) => parts.join(" · "),
            (true, false) => preview,
            (true, true) => format!("Hosted in {provider}"),
        };
        let footer = if provider == "External file" {
            "Open in browser".to_owned()
        } else {
            format!("Open in {provider}")
        };
        Card {
            author: provider,
            title,
            title_link: link,
            text,
            fields: Vec::new(),
            footer,
            image: None,
            thumbnail: None,
            color: Some(0x1d_9bd1),
            sponsored: false,
            actions: Vec::new(),
        }
    }

    /// Maps a legacy Slack Post (`mode == "post"`, `filetype` `post`/`space`) into a read-only card.
    fn post_card(&self, file: &Value, fallback_link: &str) -> Card {
        let title = first(&[&file["title"], &file["name"]]).unwrap_or("Slack Post").trim().to_owned();
        let link = first(&[&file["permalink"], &file["url_private"]])
            .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
            .unwrap_or(fallback_link)
            .to_owned();
        let text = first(&[&file["preview_plain_text"], &file["plain_text"], &file["preview"], &file["preview_highlight"]])
            .map(|raw| if raw.contains('<') && raw.contains('>') { strip_html(raw) } else { self.words(raw) })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Shared Slack post".to_owned());
        Card {
            author: "Slack Post".to_owned(),
            title,
            title_link: link,
            text,
            fields: Vec::new(),
            footer: "Open in Slack".to_owned(),
            image: None,
            thumbnail: None,
            color: Some(0x4a_154b),
            sponsored: false,
            actions: Vec::new(),
        }
    }

    /// Extracts optional clip transcript preview/status and registers the lazy WebVTT caption track
    /// (`{id}:vtt`) when Slack serves it. Never fetches the VTT eagerly.
    fn clip_transcript(&self, id: &str, file: &Value, media: &mut Registered) -> Option<Transcript> {
        let tr = &file["transcription"];
        let status = tr["status"].as_str().unwrap_or("").trim().to_owned();
        let preview = tr["preview"]["content"]
            .as_str()
            .or_else(|| tr["preview"].as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        let vtt_file = file["vtt"]
            .as_str()
            .filter(|u| !u.is_empty() && self.hosted(u))
            .map(|u| {
                let vtt = MediaFile::new(format!("{id}:vtt"), MediaKind::File);
                media.push((vtt.clone(), u.to_owned()));
                vtt
            });
        let transcript = Transcript {
            preview,
            status,
            file: vtt_file,
            lines: Vec::new(),
        };
        (!transcript.is_empty()).then_some(transcript)
    }

    /// An upload as an attachment: the file itself and the picture a row shows inline. Both
    /// addresses need the session to fetch (see `Command::Download`).
    ///
    /// The picture a row shows is registered at the size a row normally draws, and every other
    /// size Slack made of it as a variant, so a 2x screen is given one large enough.
    /// For video clips, Slack's smaller transcoded `mp4` copy is preferred over the original
    /// `url_private` when hosted by Slack.
    pub(super) fn file(&self, file: &Value, media: &mut Registered, variants: &mut Sizes) -> Option<Attachment> {
        let id = file["id"].as_str()?;
        let subtype = file["subtype"].as_str().unwrap_or("");
        let raw_mime = file["mimetype"].as_str().unwrap_or("");
        let kind = match raw_mime.split_once('/') {
            Some(("image", "gif")) => MediaKind::Gif,
            Some(("image", _)) => MediaKind::Image,
            Some(("video", _)) => MediaKind::Video,
            Some(("audio", _)) => MediaKind::Audio,
            _ => match subtype {
                "slack_audio" => MediaKind::Audio,
                "slack_video" => MediaKind::Video,
                _ => MediaKind::File,
            },
        };
        let mime = if !raw_mime.is_empty() {
            raw_mime
        } else {
            match subtype {
                "slack_audio" => "audio/ogg",
                "slack_video" => "video/mp4",
                _ => "",
            }
        };
        // Prefer Slack's smaller transcoded MP4 copy over the raw upload for video clips,
        // falling back to `url_private` (and `hls` if neither is present).
        let url = if kind == MediaKind::Video {
            [&file["mp4"], &file["url_private"], &file["hls"]]
                .into_iter()
                .filter_map(Value::as_str)
                .find(|u| !u.is_empty() && self.hosted(u))?
        } else {
            file["url_private"].as_str().filter(|url| self.hosted(url))?
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
            MediaKind::File => still("thumb_pdf", 0)
                .or_else(|| still("thumb_480", 480))
                .or_else(|| still("thumb_360", 360))
                .into_iter()
                .collect::<Vec<_>>(),
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
        let duration_ms = file["duration_ms"].as_u64().unwrap_or(0);
        let duration = if duration_ms > 0 { (duration_ms / 1000).max(1) as u32 } else { 0 };
        let transcript = self.clip_transcript(id, file, media);
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
            duration,
            round: false,
            view_once: false,
            sticker_set: String::new(),
            transcription: None,
            transcript,
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
    fn framed(&self, card: &Value, rich: &mut Rich, media: &mut Registered) -> Option<Card> {
        let words = |value: &Value| self.words(value.as_str().unwrap_or(""));
        let fields = card["fields"].as_array().into_iter().flatten().take(CARD_FIELDS);
        let hosted_image = |url: Option<&str>, w: &Value, h: &Value, media: &mut Registered| -> Option<MediaFile> {
            let url = url.filter(|u| self.hosted(u))?;
            let file = MediaFile {
                width: pixels(w),
                height: pixels(h),
                ..MediaFile::new(format!("card:{:x}", fnv(url)), MediaKind::Image)
            };
            media.push((file.clone(), url.to_owned()));
            Some(file)
        };
        let image = hosted_image(first(&[&card["image_url"]]), &card["image_width"], &card["image_height"], media);
        let thumbnail = hosted_image(first(&[&card["thumb_url"]]), &card["thumb_width"], &card["thumb_height"], media);
        let mut text = words(&card["text"]);
        let mut card_fields: Vec<(String, String)> = fields.map(|field| (words(&field["title"]), words(&field["value"]))).collect();
        if let Some(blocks) = card["blocks"].as_array() {
            let mut inner = Rich::new(self);
            inner.blocks(blocks);
            inner.finish();
            if !inner.text.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&inner.text);
            }
            for block in blocks {
                if block["type"] == "section" {
                    for f in block["fields"].as_array().into_iter().flatten() {
                        if card_fields.len() >= CARD_FIELDS {
                            break;
                        }
                        if let Some(val) = f["text"].as_str().filter(|s| !s.is_empty()) {
                            let w = self.words(val);
                            if let Some((k, v)) = w.split_once('\n') {
                                card_fields.push((k.trim().to_owned(), v.trim().to_owned()));
                            } else {
                                card_fields.push((String::new(), w));
                            }
                        }
                    }
                }
            }
        }
        let mut framed = Card {
            author: words(&card["author_name"]),
            title: words(&card["title"]),
            title_link: first(&[&card["title_link"]]).unwrap_or("").to_owned(),
            text,
            fields: card_fields,
            footer: words(&card["footer"]),
            image,
            thumbnail,
            color: colour(&card["color"]),
            sponsored: false,
            ..Card::default()
        };
        if framed.title.is_empty() && framed.text.is_empty() && framed.fields.is_empty() && framed.image.is_none() {
            framed.text = words(&card["fallback"]);
        }
        if framed.title.is_empty() && framed.text.is_empty() && framed.fields.is_empty() && framed.image.is_none() {
            return None;
        }
        if let Some(pretext) = first(&[&card["pretext"]]) {
            rich.line();
            rich.mrkdwn(pretext);
        }
        Some(framed)
    }

    /// Maps a Slack Canvas file (`filetype` `quip` / `canvas` / `slack_canvas`) into a structured
    /// readable card with an "Open in Slack" link instead of a bare download chip.
    fn canvas_card(&self, file: &Value, fallback_link: &str) -> Card {
        let title = first(&[&file["title"], &file["name"]]).unwrap_or("Untitled Canvas").trim().to_owned();
        let title = if title.is_empty() { "Untitled Canvas".to_owned() } else { title };
        let link = first(&[&file["permalink"], &file["url_private"]])
            .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
            .unwrap_or(fallback_link)
            .to_owned();
        let creator = file["user"]
            .as_str()
            .filter(|id| !id.is_empty())
            .and_then(|id| self.cached(id))
            .map(|u| u.name);
        let author = match creator {
            Some(who) if !who.is_empty() => format!("Slack Canvas · {who}"),
            _ => "Slack Canvas".to_owned(),
        };

        let mut text = String::new();
        let mut fields: Vec<(String, String)> = Vec::new();

        // Structured sections when present in the payload.
        if let Some(sections) = file["sections"].as_array() {
            for sec in sections {
                if fields.len() >= CARD_FIELDS {
                    break;
                }
                let heading = first(&[&sec["title"], &sec["heading"]]).map(strip_html).unwrap_or_default();
                let body = first(&[&sec["text"], &sec["content"], &sec["markdown"], &sec["body"]])
                    .map(|raw| {
                        if raw.contains('<') && raw.contains('>') {
                            strip_html(raw)
                        } else {
                            self.words(raw)
                        }
                    })
                    .unwrap_or_default();
                if !heading.is_empty() || !body.is_empty() {
                    fields.push((heading, body));
                }
            }
        }

        // Canvas Block Kit document content when present.
        if let Some(blocks) = file["blocks"].as_array().filter(|b| !b.is_empty()) {
            let mut inner = Rich::new(self);
            inner.blocks(blocks);
            inner.finish();
            if !inner.text.is_empty() {
                text = inner.text;
            }
        }

        // Plain/HTML preview text from Slack's file payload.
        if text.is_empty() {
            if let Some(raw) = first(&[
                &file["preview_plain_text"],
                &file["plain_text"],
                &file["document_content"],
                &file["preview"],
                &file["preview_highlight"],
            ]) {
                let cleaned = if raw.contains('<') && raw.contains('>') {
                    strip_html(raw)
                } else {
                    self.words(raw)
                };
                if !cleaned.is_empty() {
                    text = cleaned;
                }
            }
        }

        if text.is_empty() && fields.is_empty() {
            text = if file["is_huddle_notes"] == true || title.to_ascii_lowercase().contains("huddle") {
                "Huddle notes canvas".to_owned()
            } else {
                "Shared Slack canvas".to_owned()
            };
        }

        Card {
            author,
            title,
            title_link: link,
            text,
            fields,
            footer: "Open in Slack".to_owned(),
            image: None,
            thumbnail: None,
            color: Some(0x4a_154b),
            sponsored: false,
            actions: Vec::new(),
        }
    }

    /// Maps a Slack List file (`filetype` `list` / `slack_list`) into a structured readable card
    /// with columns/items summary and an "Open in Slack" link instead of a bare download chip.
    fn list_card(&self, file: &Value, fallback_link: &str) -> Card {
        let title = first(&[&file["title"], &file["name"]]).unwrap_or("Untitled List").trim().to_owned();
        let title = if title.is_empty() { "Untitled List".to_owned() } else { title };
        let link = first(&[&file["permalink"], &file["url_private"]])
            .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
            .unwrap_or(fallback_link)
            .to_owned();
        let creator = file["user"]
            .as_str()
            .filter(|id| !id.is_empty())
            .and_then(|id| self.cached(id))
            .map(|u| u.name);
        let author = match creator {
            Some(who) if !who.is_empty() => format!("Slack List · {who}"),
            _ => "Slack List".to_owned(),
        };

        let meta = &file["list_metadata"];
        let mut text = first(&[
            &meta["description"],
            &file["preview_plain_text"],
            &file["plain_text"],
            &file["preview"],
        ])
        .map(|raw| if raw.contains('<') && raw.contains('>') { strip_html(raw) } else { self.words(raw) })
        .unwrap_or_default();

        let mut fields: Vec<(String, String)> = Vec::new();
        let items = meta["items"]
            .as_array()
            .or_else(|| file["items"].as_array())
            .or_else(|| file["rows"].as_array());
        if let Some(items) = items {
            for item in items.iter().take(CARD_FIELDS) {
                let item_title = first(&[&item["title"], &item["name"], &item["text"]])
                    .map(strip_html)
                    .unwrap_or_default();
                let mut parts: Vec<String> = Vec::new();
                if let Some(status) = first(&[&item["status"], &item["state"]]) {
                    parts.push(strip_html(status));
                }
                if let Some(assignee) = first(&[&item["assignee"], &item["owner"]]) {
                    let who = self.cached(assignee).map(|u| u.name).unwrap_or_else(|| strip_html(assignee));
                    if !who.is_empty() {
                        parts.push(who);
                    }
                }
                if let Some(due) = first(&[&item["due_date"], &item["due"]]) {
                    parts.push(format!("Due {}", strip_html(due)));
                }
                if !item_title.is_empty() || !parts.is_empty() {
                    let label = if item_title.is_empty() { "Item".to_owned() } else { item_title };
                    fields.push((label, parts.join(" · ")));
                }
            }
        }

        if let Some(schema) = meta["schema"].as_array().filter(|s| !s.is_empty()) {
            let cols: Vec<String> = schema
                .iter()
                .filter_map(|c| first(&[&c["name"], &c["label"], &c["key"]]).map(strip_html))
                .filter(|s| !s.is_empty())
                .take(8)
                .collect();
            if !cols.is_empty() {
                let col_line = format!("Columns: {}", cols.join(", "));
                if text.is_empty() {
                    text = col_line;
                } else {
                    text.push('\n');
                    text.push_str(&col_line);
                }
            }
        }

        if text.is_empty() && fields.is_empty() {
            text = "Shared Slack list".to_owned();
        }

        Card {
            author,
            title,
            title_link: link,
            text,
            fields,
            footer: "Open in Slack".to_owned(),
            image: None,
            thumbnail: None,
            color: Some(0x2e_7d6f),
            sponsored: false,
            actions: Vec::new(),
        }
    }

    /// Extracts hosted image blocks, section image accessories, and video blocks from Block Kit.
    fn block_media(&self, blocks: &[Value], media: &mut Registered, attachments: &mut Vec<Attachment>, previews: &mut Vec<Preview>) {
        for block in blocks {
            match block["type"].as_str().unwrap_or("") {
                "image" => {
                    let url = first(&[&block["image_url"], &block["slack_file"]["url_private"]]);
                    if let Some(url) = url.filter(|u| self.hosted(u)) {
                        let name = first(&[&block["title"]["text"], &block["alt_text"]]).unwrap_or("image").to_owned();
                        let file = MediaFile {
                            width: pixels(&block["image_width"]),
                            height: pixels(&block["image_height"]),
                            ..MediaFile::new(format!("blockimg:{:x}", fnv(url)), MediaKind::Image)
                        };
                        media.push((file.clone(), url.to_owned()));
                        attachments.push(Attachment::new(name, file));
                    }
                }
                "section" => {
                    let acc = &block["accessory"];
                    if acc["type"] == "image" {
                        if let Some(url) = first(&[&acc["image_url"]]).filter(|u| self.hosted(u)) {
                            let name = first(&[&acc["alt_text"]]).unwrap_or("thumbnail").to_owned();
                            let file = MediaFile::new(format!("blockacc:{:x}", fnv(url)), MediaKind::Image);
                            media.push((file.clone(), url.to_owned()));
                            attachments.push(Attachment::new(name, file));
                        }
                    }
                }
                "video" => {
                    let url = first(&[&block["title_url"], &block["video_url"]]).unwrap_or("");
                    if !url.is_empty() {
                        let image = first(&[&block["thumbnail_url"]]).filter(|u| self.hosted(u)).map(|u| {
                            let file = MediaFile::new(format!("video:{:x}", fnv(u)), MediaKind::Image);
                            media.push((file.clone(), u.to_owned()));
                            file
                        });
                        previews.push(Preview {
                            url: url.to_owned(),
                            site: first(&[&block["provider_name"], &block["author_name"]]).unwrap_or("Video").to_owned(),
                            title: first(&[&block["title"]["text"], &block["alt_text"]]).unwrap_or("Video").to_owned(),
                            description: first(&[&block["description"]["text"]]).unwrap_or("").chars().take(DESCRIPTION).collect(),
                            image,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// A message shared from elsewhere ("Share message"): where it came from, and what it said.
    /// When the forwarder also wrote a note (`rich.text` is non-empty), the note stays in
    /// `Message::text` and the forwarded message's body sits in `Forwarded::text` so both stay distinct.
    fn forwarded(&self, card: &Value, rich: &mut Rich, media: &mut Registered, variants: &mut Sizes, attachments: &mut Vec<Attachment>) -> Forwarded {
        let mut quoted = Rich::new(self);
        let blocks = card["message_blocks"][0]["message"]["blocks"].as_array();
        if !blocks.is_some_and(|blocks| quoted.blocks(blocks)) {
            quoted.mrkdwn(first(&[&card["text"], &card["fallback"]]).unwrap_or(""));
        }
        quoted.finish();
        let has_note = !rich.text.trim().is_empty();
        if !has_note && !quoted.text.is_empty() {
            rich.text = quoted.text.clone();
            rich.spans = quoted.spans;
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
            author: first(&[&card["author_name"]]).unwrap_or("").to_owned(),
            text: quoted.text,
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

    /// Parses a Slack App Home tab's Block Kit `blocks` array into rich text, spans, and
    /// interactive controls, reusing the same Block Kit parser as messages.
    pub(super) fn parse_app_home_blocks(
        &self,
        blocks: &[Value],
    ) -> (String, Vec<Span>, Vec<Card>, Vec<MessageControl>) {
        let mut rich = Rich::new(self);
        rich.blocks(blocks);
        rich.finish();
        let mut controls = Vec::new();
        collect_block_controls(blocks, &mut controls);
        controls.truncate(MESSAGE_CONTROLS_LIMIT);
        (rich.text, rich.spans, Vec::new(), controls)
    }

    /// A message event, or one entry of a page of history. `None` only for what Slack itself
    /// hides. Registers every file the message refers to.
    pub(super) fn message(&self, channel: &str, event: &Value) -> Option<Message> {
        let ts = event["ts"].as_str()?;
        let subtype = event["subtype"].as_str();
        let ephemeral = event["is_ephemeral"] == true
            || subtype == Some("ephemeral")
            || event["message_type"].as_str() == Some("ephemeral")
            || event["ephemeral_type"].is_string();
        let handled_apart = matches!(subtype, Some("message_changed" | "message_deleted" | "message_replied"));
        if handled_apart || (event["hidden"] == true && !ephemeral) {
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
        let (mut framed, mut extra_cards, mut forwarded) = (None, Vec::new(), None);
        if let Some(blocks) = event["blocks"].as_array() {
            self.block_media(blocks, &mut media, &mut attachments, &mut previews);
        }
        for card in event["attachments"].as_array().into_iter().flatten() {
            if (card["is_share"] == true || card["is_msg_unfurl"] == true) && forwarded.is_none() {
                forwarded = Some(Box::new(self.forwarded(card, &mut rich, &mut media, &mut variants, &mut attachments)));
                continue;
            }
            let is_rich_card = card["fields"].as_array().is_some_and(|f| !f.is_empty())
                || card["blocks"].as_array().is_some_and(|b| !b.is_empty())
                || card["color"].as_str().is_some_and(|c| !c.is_empty());
            match first(&[&card["from_url"], &card["original_url"]]) {
                Some(url) if !is_rich_card => previews.push(self.preview(card, url, &mut media)),
                _ => match self.framed(card, &mut rich, &mut media) {
                    Some(c) => {
                        if framed.is_none() {
                            framed = Some(Box::new(c));
                        } else {
                            extra_cards.push(c);
                        }
                    }
                    None => match first(&[&card["from_url"], &card["original_url"], &card["title_link"]]) {
                        Some(url) => previews.push(self.preview(card, url, &mut media)),
                        None => self.card(card, &mut rich),
                    },
                },
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
            if is_canvas(file) {
                push_card(&mut framed, &mut extra_cards, self.canvas_card(file, &link));
                continue;
            }
            if is_list(file) {
                push_card(&mut framed, &mut extra_cards, self.list_card(file, &link));
                continue;
            }
            if is_post(file) {
                push_card(&mut framed, &mut extra_cards, self.post_card(file, &link));
                continue;
            }
            if self.is_external(file) {
                push_card(&mut framed, &mut extra_cards, self.external_file_card(file, &link));
                continue;
            }
            if is_snippet(file) {
                append_snippet_preview(file, &mut rich);
            }
            match self.file(file, &mut media, &mut variants) {
                Some(attachment) => attachments.push(attachment),
                // Fallback for minimal file stubs without a hosted address (e.g. pinned file stubs).
                None => {
                    rich.line();
                    let target = first(&[&file["url_private"], &file["permalink"]]).unwrap_or(&link);
                    rich.link(target, Some(name));
                }
            }
        }
        let controls = parse_controls(event);
        if !controls.is_empty() {
            if let Some(app_id) = first(&[&event["app_id"], &event["bot_id"]]) {
                let mut state = self.state();
                if state.message_apps.len() >= 512 {
                    state.message_apps.clear();
                }
                state.message_apps.insert((channel.to_owned(), ts.to_owned()), app_id.to_owned());
            }
        }
        if subtype == Some("channel_canvas_updated") && rich.text.trim().is_empty() {
            rich.text = "Updated the channel canvas".to_owned();
        }
        if rich.undrawn && controls.is_empty() && !rich.text.is_empty() {
            // Controls we could not parse still link out to Slack.
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
            // The payload names everyone: the ids the shell draws faces and names from.
            let who: Vec<String> = users
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(protocol::REACTION_REACTORS)
                .map(str::to_owned)
                .collect();
            Some(Reaction {
                emoji,
                file,
                count: reaction["count"].as_u64().or(users.map(|users| users.len() as u64)).unwrap_or(1) as u32,
                mine: users.is_some_and(|users| users.iter().any(|user| user == mine)),
                who,
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
        let (suppress_broadcast, keyword_hit) = {
            let state = self.state();
            let muted = state.muted.contains(channel);
            let suppress = muted || state.suppress_at_channel.contains(channel);
            let kw = !muted && !outgoing && super::tree::matches_keyword(&rich.text, &state.highlight_words);
            (suppress, kw)
        };
        message.mentions_me = !outgoing && (rich.mentioned || (rich.broadcast && !suppress_broadcast) || keyword_hit);
        let empty = rich.text.is_empty() && attachments.is_empty() && previews.is_empty() && framed.is_none() && controls.is_empty();
        // What Slack leaves of a deleted message that a thread still hangs off.
        let tombstone = subtype == Some("tombstone");
        message.kind = if tombstone {
            MessageKind::Normal
        } else if empty {
            // Never blank, never dropped: one line and the way to the real thing.
            let what = if rich.undrawn { "An interactive message" } else { "A message tinyclient cannot show yet" };
            rich.text = format!("{what} from {}", sender.name);
            MessageKind::Unsupported { link: link.clone() }
        } else if system || event["room"].is_object() {
            MessageKind::System
        } else {
            MessageKind::Normal
        };
        message.permalink = link;
        message.text = rich.text;
        message.spans = rich.spans;
        message.sender = sender;
        message.attachments = attachments;
        message.previews = previews;
        message.reactions = reactions;
        message.thread = summary;
        message.card = framed;
        message.cards = extra_cards;
        message.controls = controls;
        message.ephemeral = ephemeral;
        message.forwarded = forwarded;
        message.edited = event["edited"].is_object();
        message.edited_at = event["edited"]["ts"].as_str().map(seconds).filter(|at| *at > 0);
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
