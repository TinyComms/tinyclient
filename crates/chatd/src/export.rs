//! Local-only export of cached conversations (`messages.json`, `index.html`, and on-disk media).
//!
//! Exports only what is already in the local SQLite store and media cache: never pages a network's
//! history, never exports member lists, omits expired disappearing messages, and marks every
//! exported file with the OS download quarantine helper (`protocol::quarantine::mark`).

use crate::store::Store;
use protocol::{Conversation, ExportScope, MediaKind, Message};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Summary returned by [`run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub path: PathBuf,
    pub conversations: u32,
    pub messages: u64,
    pub files: u64,
    pub earliest: Option<i64>,
    pub latest: Option<i64>,
}

/// Stable, documented JSON envelope written to `messages.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportDocument {
    pub version: u32,
    pub exported_at: i64,
    pub notice: String,
    pub conversation: ExportConversation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earliest: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<i64>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportConversation {
    pub network: protocol::Network,
    pub account: String,
    pub id: String,
    pub title: String,
    pub kind: protocol::ConversationKind,
}

const UNENCRYPTED_NOTICE: &str = "Unencrypted local cache export. Includes messages that were end-to-end encrypted in transit. Only messages already cached on this device are included; member lists are omitted.";

fn sanitize_component(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect();
    let trimmed = cleaned.trim_matches(['_', '.']);
    if trimmed.is_empty() {
        "item".to_owned()
    } else {
        trimmed.chars().take(64).collect()
    }
}

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn render_conversation_html(
    conv: &Conversation,
    messages: &[Message],
    copied_media: &std::collections::HashMap<String, String>,
    earliest: Option<i64>,
    latest: Option<i64>,
) -> String {
    let title = if conv.title.is_empty() { &conv.key.id } else { &conv.title };
    let range = match (earliest, latest) {
        (Some(a), Some(b)) => format!("Cached messages from unix {a} to {b} ({} messages).", messages.len()),
        _ => "No cached messages in this range.".to_owned(),
    };
    let mut html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n<style>\
         body{{font-family:system-ui,-apple-system,sans-serif;max-width:820px;margin:24px auto;padding:0 16px;color:#111;background:#fafafa;}}\
         .notice{{background:#fff3cd;border:1px solid #ffe69c;padding:10px 12px;border-radius:6px;margin-bottom:16px;font-size:13px;}}\
         .meta{{color:#666;font-size:13px;margin-bottom:18px;}}\
         .msg{{background:#fff;border:1px solid #e5e7eb;border-radius:8px;padding:10px 12px;margin-bottom:10px;}}\
         .head{{display:flex;gap:8px;align-items:baseline;margin-bottom:4px;}}\
         .sender{{font-weight:600;}}\
         .time{{color:#666;font-size:12px;}}\
         .text{{white-space:pre-wrap;word-break:break-word;}}\
         .media{{margin-top:8px;display:flex;flex-wrap:wrap;gap:8px;}}\
         .media img{{max-width:360px;max-height:280px;border-radius:6px;border:1px solid #e5e7eb;}}\
         </style>\n</head>\n<body>\n<h1>{}</h1>\n<div class=\"notice\">{}</div>\n<div class=\"meta\">{} &middot; {}</div>\n",
        escape_html(title),
        escape_html(title),
        escape_html(UNENCRYPTED_NOTICE),
        escape_html(conv.key.network.label()),
        escape_html(&range),
    );
    for m in messages {
        let sender = m.shown_sender();
        html.push_str("<div class=\"msg\"><div class=\"head\"><span class=\"sender\">");
        html.push_str(&escape_html(sender));
        html.push_str("</span><span class=\"time\">");
        html.push_str(&m.sent.to_string());
        html.push_str("</span></div>");
        if !m.text.is_empty() {
            html.push_str("<div class=\"text\">");
            html.push_str(&escape_html(&m.text));
            html.push_str("</div>");
        }
        if !m.attachments.is_empty() {
            html.push_str("<div class=\"media\">");
            for att in &m.attachments {
                if let Some(rel) = copied_media.get(&att.file.id) {
                    if matches!(att.file.kind, MediaKind::Image | MediaKind::Gif) || att.sticker {
                        html.push_str(&format!(
                            "<a href=\"{0}\"><img src=\"{0}\" alt=\"{1}\"></a>",
                            escape_html(rel),
                            escape_html(&att.name),
                        ));
                    } else {
                        let label = if att.name.is_empty() { rel.as_str() } else { att.name.as_str() };
                        html.push_str(&format!(
                            "<a href=\"{}\">{}</a>",
                            escape_html(rel),
                            escape_html(label),
                        ));
                    }
                }
            }
            html.push_str("</div>");
        }
        html.push_str("</div>\n");
    }
    html.push_str("</body>\n</html>\n");
    html
}

fn export_one(
    store: &Store,
    conv: &Conversation,
    dir: &Path,
    since: Option<i64>,
    until: Option<i64>,
    include_files: bool,
    now: i64,
) -> Result<(u64, u64, Option<i64>, Option<i64>), String> {
    fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let messages = store
        .export_messages(&conv.key, since, until, now)
        .map_err(|e| format!("read cached messages: {e}"))?;
    let earliest = messages.first().map(|m| m.sent);
    let latest = messages.last().map(|m| m.sent);
    let mut copied_media = std::collections::HashMap::new();
    let mut files: u64 = 0;
    if include_files {
        let media_dir = dir.join("media");
        for m in &messages {
            for (ix, att) in m.attachments.iter().enumerate() {
                if copied_media.contains_key(&att.file.id) {
                    continue;
                }
                let Some(src) = att.file.path.as_deref() else {
                    continue;
                };
                let src_path = Path::new(src);
                if !src_path.is_file() {
                    continue;
                }
                fs::create_dir_all(&media_dir).map_err(|e| format!("create media dir: {e}"))?;
                let base = if !att.name.is_empty() {
                    sanitize_component(&att.name)
                } else {
                    src_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(sanitize_component)
                        .unwrap_or_else(|| format!("{}_{ix}", sanitize_component(&att.file.id)))
                };
                let filename = format!("{}_{}", sanitize_component(&att.file.id), base);
                let dest = media_dir.join(&filename);
                fs::copy(src_path, &dest).map_err(|e| format!("copy {}: {e}", src_path.display()))?;
                let _ = protocol::quarantine::mark(&dest);
                copied_media.insert(att.file.id.clone(), format!("media/{filename}"));
                files += 1;
            }
        }
    }
    let doc = ExportDocument {
        version: 1,
        exported_at: now,
        notice: UNENCRYPTED_NOTICE.to_owned(),
        conversation: ExportConversation {
            network: conv.key.network,
            account: conv.key.account.clone(),
            id: conv.key.id.clone(),
            title: conv.title.clone(),
            kind: conv.kind,
        },
        earliest,
        latest,
        messages,
    };
    let json_path = dir.join("messages.json");
    let json = serde_json::to_string_pretty(&doc).map_err(|e| format!("serialize messages.json: {e}"))?;
    fs::write(&json_path, json).map_err(|e| format!("write messages.json: {e}"))?;
    let _ = protocol::quarantine::mark(&json_path);

    let html_path = dir.join("index.html");
    let html = render_conversation_html(conv, &doc.messages, &copied_media, earliest, latest);
    fs::write(&html_path, html).map_err(|e| format!("write index.html: {e}"))?;
    let _ = protocol::quarantine::mark(&html_path);

    Ok((doc.messages.len() as u64, files, earliest, latest))
}

/// Runs a local cache export into `destination`.
pub fn run(
    store: &Store,
    scope: &ExportScope,
    destination: &Path,
    since: Option<i64>,
    until: Option<i64>,
    include_files: bool,
    now: i64,
) -> Result<Summary, String> {
    let conversations = store
        .export_conversations(scope)
        .map_err(|e| format!("read cached conversations: {e}"))?;
    if conversations.is_empty() {
        return Err("No cached conversations matched that export scope.".to_owned());
    }
    fs::create_dir_all(destination).map_err(|e| format!("create {}: {e}", destination.display()))?;

    let mut total_messages: u64 = 0;
    let mut total_files: u64 = 0;
    let mut earliest: Option<i64> = None;
    let mut latest: Option<i64> = None;

    if matches!(scope, ExportScope::Conversation { .. }) && conversations.len() == 1 {
        let (msgs, files, e, l) = export_one(store, &conversations[0], destination, since, until, include_files, now)?;
        return Ok(Summary {
            path: destination.to_path_buf(),
            conversations: 1,
            messages: msgs,
            files,
            earliest: e,
            latest: l,
        });
    }

    let mut index_links = Vec::new();
    for conv in &conversations {
        let acct_dir = format!(
            "{}-{}",
            sanitize_component(conv.key.network.label()),
            sanitize_component(&conv.key.account)
        );
        let conv_dir = format!(
            "{}-{}",
            sanitize_component(&conv.title),
            sanitize_component(&conv.key.id)
        );
        let sub = destination.join(&acct_dir).join(&conv_dir);
        let (msgs, files, e, l) = export_one(store, conv, &sub, since, until, include_files, now)?;
        total_messages += msgs;
        total_files += files;
        if let Some(e) = e {
            earliest = Some(earliest.map_or(e, |cur| cur.min(e)));
        }
        if let Some(l) = l {
            latest = Some(latest.map_or(l, |cur| cur.max(l)));
        }
        let label = if conv.title.is_empty() { &conv.key.id } else { &conv.title };
        index_links.push(format!(
            "<li><a href=\"{acct_dir}/{conv_dir}/index.html\">{} ({})</a> &mdash; {msgs} messages</li>",
            escape_html(label),
            escape_html(conv.key.network.label()),
        ));
    }

    let root_html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head><meta charset=\"utf-8\"><title>TinyClient Export</title></head>\n\
         <body><h1>TinyClient Cached Export</h1><p>{}</p><ul>{}</ul></body>\n</html>\n",
        escape_html(UNENCRYPTED_NOTICE),
        index_links.join("\n"),
    );
    let root_index = destination.join("index.html");
    fs::write(&root_index, root_html).map_err(|e| format!("write index.html: {e}"))?;
    let _ = protocol::quarantine::mark(&root_index);

    Ok(Summary {
        path: destination.to_path_buf(),
        conversations: conversations.len() as u32,
        messages: total_messages,
        files: total_files,
        earliest,
        latest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ConversationRow, Store};
    use protocol::{AccountKey, Attachment, ConversationKind, Key, MediaFile, MediaKind, Network};

    #[test]
    fn exports_conversation_json_html_and_media_and_skips_expired_disappearing() {
        let tmp = std::env::temp_dir().join(format!("tinyclient-export-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let mut store = Store::open(&tmp.join("test.db")).unwrap();
        let account = AccountKey { network: Network::Discord, account: "a1".into() };
        store.set_account(&account, "Me", "u1").unwrap();
        store
            .upsert_conversation(&account, &ConversationRow::new("c1", ConversationKind::Channel, "general"))
            .unwrap();
        let conv = Key::new(&account, "c1");

        // Create a real image file on disk to test media export + quarantine mark.
        let img = tmp.join("photo.png");
        fs::write(&img, b"\x89PNG\r\n\x1a\n").unwrap();

        let now = 2_000;
        for i in 1..=50 {
            let mut m = Message::plain(format!("m{i}"), "Mara", format!("message #{i}"), 1_000 + i, false);
            if i == 50 {
                let mut file = MediaFile::new("f1", MediaKind::Image);
                file.path = Some(img.to_string_lossy().into_owned());
                file.bytes = 8;
                m.attachments.push(Attachment::new("photo.png", file));
            }
            store.ingest(&conv, &m, false).unwrap();
        }
        // Expired disappearing message must NOT be exported.
        let mut expired = Message::plain("m_exp", "Mara", "secret gone", 1_500, false);
        expired.expires_at = Some(now - 10);
        store.ingest(&conv, &expired, false).unwrap();

        let out_dir = tmp.join("out");
        let summary = run(
            &store,
            &ExportScope::Conversation { conversation: conv.clone() },
            &out_dir,
            None,
            None,
            true,
            now,
        )
        .unwrap();

        assert_eq!(summary.conversations, 1);
        assert_eq!(summary.messages, 50);
        assert_eq!(summary.files, 1);
        assert_eq!(summary.earliest, Some(1_001));
        assert_eq!(summary.latest, Some(1_050));

        // messages.json validates against the documented ExportDocument + protocol::Message shape.
        let raw_json = fs::read_to_string(out_dir.join("messages.json")).unwrap();
        let parsed: ExportDocument = serde_json::from_str(&raw_json).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.messages.len(), 50);
        assert!(parsed.messages.iter().all(|m| m.id != "m_exp"));

        // index.html has the messages, unencrypted warning, and image link.
        let html = fs::read_to_string(out_dir.join("index.html")).unwrap();
        assert!(html.contains("Unencrypted local cache export"));
        assert!(html.contains("message #50"));
        assert!(!html.contains("secret gone"));
        assert!(html.contains("media/f1_photo.png"));
        assert!(out_dir.join("media").join("f1_photo.png").is_file());

        // Date range + include_files=false
        let out_range = tmp.join("out_range");
        let ranged = run(
            &store,
            &ExportScope::Conversation { conversation: conv },
            &out_range,
            Some(1_041),
            Some(1_050),
            false,
            now,
        )
        .unwrap();
        assert_eq!(ranged.messages, 10);
        assert_eq!(ranged.files, 0);

        let _ = fs::remove_dir_all(&tmp);
    }
}
