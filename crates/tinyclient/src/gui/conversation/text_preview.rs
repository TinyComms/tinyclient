//! Inline previews for text and code file attachments across all networks.
//!
//! Only `text/*` (excluding `text/html`) and a bounded allowlist of plain-text/code extensions are
//! previewed. At most [`MAX_PREVIEW_BYTES`] (100 KB) are read; binary-looking bytes (NUL or a high
//! ratio of non-whitespace control bytes) are refused so the attachment stays a plain file row.

use protocol::{Attachment, MediaKind};
use std::io::Read;
use std::path::Path;

/// Maximum bytes read from disk for an inline preview (matches `protocol::TEXT_PREVIEW_MAX_BYTES`).
pub const MAX_PREVIEW_BYTES: usize = protocol::TEXT_PREVIEW_MAX_BYTES as usize;
/// Maximum lines shown inline when unfolded (further lines open in the full-screen text viewer).
pub const MAX_INLINE_LINES: usize = 50;
/// Lines shown in the initial folded inline state (matches message code blocks).
pub const FOLD_LINES: usize = 14;
/// Prefix used by chatd's media cache for ranged head previews.
pub const HEAD_PREFIX: &str = "head:";

/// Cache id of an attachment's ranged head preview.
pub fn head_id(file_id: &str) -> String {
    format!("{HEAD_PREFIX}{file_id}")
}

/// Decoded inline preview of a text/code attachment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextPreview {
    /// Highlight language hint derived from the file extension (may be empty for plain text).
    pub language: &'static str,
    /// Full text decoded from the fetched head (up to 100 KB), for Copy and the viewer.
    pub head_text: String,
    /// Up to [`MAX_INLINE_LINES`] lines for inline display.
    pub inline_text: String,
    /// Total lines present in `inline_text`.
    pub inline_lines: usize,
    /// True when the file had more lines than [`MAX_INLINE_LINES`] or more bytes than fetched.
    pub truncated: bool,
}

/// Whether `attachment` is eligible for an inline text/code preview, returning its syntax
/// highlight language hint (`""` for plain text). Works across every network.
pub fn preview_language(attachment: &Attachment) -> Option<&'static str> {
    if attachment.sticker || attachment.file.kind != MediaKind::File {
        return None;
    }
    let ext = attachment
        .name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let by_ext = match ext.as_str() {
        "txt" | "log" | "csv" | "tsv" | "env" | "cfg" | "conf" | "ini" | "properties" => Some(""),
        "md" | "markdown" => Some("markdown"),
        "json" | "jsonc" | "jsonl" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "toml" => Some("toml"),
        "xml" | "plist" => Some("xml"),
        "rs" => Some("rust"),
        "py" | "pyi" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "jsx" | "tsx" => Some("tsx"),
        "go" => Some("go"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => Some("cpp"),
        "cs" => Some("csharp"),
        "java" | "kt" | "kts" => Some("java"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "swift" => Some("swift"),
        "sh" | "bash" | "zsh" => Some("bash"),
        "ps1" => Some("powershell"),
        "sql" => Some("sql"),
        "css" | "scss" => Some("css"),
        "diff" | "patch" => Some("diff"),
        _ => None,
    };
    if by_ext.is_some() {
        return by_ext;
    }
    let mime = attachment.mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if mime.starts_with("text/") && mime != "text/html" {
        return Some("");
    }
    None
}

/// Returns true if `bytes` look like binary data rather than UTF-8/ASCII text.
pub fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8 * 1024)];
    if sample.is_empty() {
        return false;
    }
    if sample.contains(&0) {
        return true;
    }
    let control = sample
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t' | 0x0C))
        .count();
    control * 10 > sample.len()
}

/// Decodes raw bytes (capped at [`MAX_PREVIEW_BYTES`]) into a [`TextPreview`]. Returns `None` if
/// the bytes are empty or look binary.
pub fn decode_preview(bytes: &[u8], declared_total_bytes: u64, language: &'static str) -> Option<TextPreview> {
    if bytes.is_empty() || looks_binary(bytes) {
        return None;
    }
    let capped = &bytes[..bytes.len().min(MAX_PREVIEW_BYTES)];
    let head_text = String::from_utf8_lossy(capped).replace("\r\n", "\n");
    if head_text.trim().is_empty() {
        return None;
    }
    let mut lines_iter = head_text.lines();
    let mut kept = Vec::with_capacity(MAX_INLINE_LINES);
    for _ in 0..MAX_INLINE_LINES {
        let Some(line) = lines_iter.next() else {
            break;
        };
        kept.push(line);
    }
    let more_lines = lines_iter.next().is_some();
    let more_bytes = bytes.len() > MAX_PREVIEW_BYTES
        || (declared_total_bytes > 0 && declared_total_bytes > capped.len() as u64);
    let inline_lines = kept.len();
    let inline_text = kept.join("\n");
    Some(TextPreview {
        language,
        head_text,
        inline_text,
        inline_lines,
        truncated: more_lines || more_bytes,
    })
}

/// Reads up to [`MAX_PREVIEW_BYTES`] + 1 bytes from `path` on disk and decodes a [`TextPreview`].
pub fn read_file_preview(path: &Path, declared_total_bytes: u64, language: &'static str) -> Option<TextPreview> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(MAX_PREVIEW_BYTES.min(8 * 1024));
    file.take((MAX_PREVIEW_BYTES + 1) as u64).read_to_end(&mut buf).ok()?;
    decode_preview(&buf, declared_total_bytes, language)
}

/// Reads a full text file on disk for the expanded viewer (capped at 4 MiB so an enormous file
/// never stalls the UI).
pub fn read_full_text(path: &Path) -> Option<(String, bool)> {
    const VIEWER_MAX_BYTES: usize = 4 * 1024 * 1024;
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take((VIEWER_MAX_BYTES + 1) as u64).read_to_end(&mut buf).ok()?;
    if looks_binary(&buf) {
        return None;
    }
    let truncated = buf.len() > VIEWER_MAX_BYTES;
    let slice = &buf[..buf.len().min(VIEWER_MAX_BYTES)];
    Some((String::from_utf8_lossy(slice).replace("\r\n", "\n"), truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MediaFile;

    fn file_att(name: &str, mime: &str) -> Attachment {
        Attachment {
            mime: mime.into(),
            ..Attachment::new(name, MediaFile::new("att:1", MediaKind::File))
        }
    }

    #[test]
    fn preview_language_matches_extensions_and_text_mime_across_networks() {
        assert_eq!(preview_language(&file_att("message.txt", "text/plain; charset=utf-8")), Some(""));
        assert_eq!(preview_language(&file_att("server.log", "")), Some(""));
        assert_eq!(preview_language(&file_att("main.rs", "application/octet-stream")), Some("rust"));
        assert_eq!(preview_language(&file_att("script.py", "")), Some("python"));
        assert_eq!(preview_language(&file_att("config.json", "application/json")), Some("json"));
        assert_eq!(preview_language(&file_att("notes.unknown", "text/plain")), Some(""));
        // Binary / non-text files and HTML pages are not auto-previewed.
        assert_eq!(preview_language(&file_att("archive.zip", "application/zip")), None);
        assert_eq!(preview_language(&file_att("page.html", "text/html")), None);
        let mut img = file_att("pic.png", "image/png");
        img.file.kind = MediaKind::Image;
        assert_eq!(preview_language(&img), None);
    }

    #[test]
    fn decode_preview_caps_50_lines_and_100kb_and_rejects_binary() {
        let log: String = (1..=80).map(|n| format!("line {n}: ok\n")).collect();
        let preview = decode_preview(log.as_bytes(), log.len() as u64, "").expect("plain log");
        assert_eq!(preview.inline_lines, MAX_INLINE_LINES);
        assert!(preview.inline_text.starts_with("line 1: ok\n"));
        assert!(preview.inline_text.ends_with("line 50: ok"));
        assert!(preview.truncated);

        // Lossy UTF-8 succeeds without panicking.
        let lossy = decode_preview(b"hello \xff world\n", 14, "").expect("lossy utf8");
        assert!(lossy.inline_text.contains("hello"));
        assert!(!lossy.truncated);

        // NUL bytes and control-heavy buffers are rejected as binary.
        assert!(decode_preview(b"ELF\x00\x01\x02\x03", 7, "").is_none());
        assert!(decode_preview(&[0x01, 0x02, 0x03, 0x04, b'a', b'b'], 6, "").is_none());

        // Over-100 KB input is capped and flagged truncated.
        let huge = vec![b'x'; MAX_PREVIEW_BYTES + 2048];
        let capped = decode_preview(&huge, huge.len() as u64, "").expect("capped");
        assert_eq!(capped.head_text.len(), MAX_PREVIEW_BYTES);
        assert!(capped.truncated);
    }
}
