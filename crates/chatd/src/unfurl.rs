//! Shared client-side link unfurler for networks whose relay does not carry link preview cards
//! (Google Messages, Tiny servers, etc.).
//!
//! Fetches HTML metadata (`og:title`, `og:site_name`, `og:description`, `<title>`,
//! `<meta name="description">`) with a strict timeout and byte cap, and caches results in memory.

use protocol::Preview;
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Maximum HTML bytes read from a page while looking for title/description metadata.
const MAX_HTML_BYTES: u64 = 256 * 1024;
/// Network timeout for a single unfurl fetch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum cached URLs kept in memory.
const CACHE_CAP: usize = 256;

static GLOBAL_CACHE: OnceLock<Mutex<HashMap<String, Option<Preview>>>> = OnceLock::new();

fn global_cache() -> &'static Mutex<HashMap<String, Option<Preview>>> {
    GLOBAL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Extracts the first `http://` or `https://` URL from message text, trimming trailing
/// sentence punctuation.
pub fn extract_url(text: &str) -> Option<String> {
    for word in text.split_whitespace() {
        let candidate = word.trim_matches(|c: char| matches!(c, '(' | '[' | '<' | '"' | '\''));
        if candidate.starts_with("https://") || candidate.starts_with("http://") {
            let trimmed = candidate
                .trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '>' | '"' | '\''));
            if let Some(rest) = trimmed
                .strip_prefix("https://")
                .or_else(|| trimmed.strip_prefix("http://"))
            {
                if !rest.is_empty() && !rest.starts_with('/') {
                    return Some(trimmed.to_owned());
                }
            }
        }
    }
    None
}

/// Fetches a link preview card for `url`, using the process-wide in-memory cache.
pub fn fetch(url: &str) -> Option<Preview> {
    fetch_with_cache(url, global_cache())
}

/// Fetches a link preview card for `url` using a caller-provided cache (handy for isolated tests).
pub fn fetch_with_cache(
    url: &str,
    cache: &Mutex<HashMap<String, Option<Preview>>>,
) -> Option<Preview> {
    {
        let guard = cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(hit) = guard.get(url) {
            return hit.clone();
        }
    }
    let result = fetch_uncached(url);
    {
        let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());
        if guard.len() >= CACHE_CAP && !guard.contains_key(url) {
            if let Some(oldest) = guard.keys().next().cloned() {
                guard.remove(&oldest);
            }
        }
        guard.insert(url.to_owned(), result.clone());
    }
    result
}

fn fetch_uncached(url: &str) -> Option<Preview> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .get(url)
        .header("User-Agent", "tinyclient-link-preview/1.0")
        .header("Accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1")
        .call()
        .ok()?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return None;
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.is_empty()
        && !content_type.contains("text/html")
        && !content_type.contains("application/xhtml")
    {
        return None;
    }
    let mut bytes = Vec::new();
    let _ = response
        .body_mut()
        .as_reader()
        .take(MAX_HTML_BYTES)
        .read_to_end(&mut bytes);
    let html = String::from_utf8_lossy(&bytes);
    parse_html(url, &html)
}

pub fn parse_html(url: &str, html: &str) -> Option<Preview> {
    let mut og_title = String::new();
    let mut tw_title = String::new();
    let mut og_site = String::new();
    let mut og_desc = String::new();
    let mut meta_desc = String::new();

    let lower = html.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find("<meta") {
        let start = pos + rel;
        let Some(end_rel) = lower[start..].find('>') else {
            break;
        };
        let end = start + end_rel + 1;
        let tag = &html[start..end];
        let tag_lower = &lower[start..end];
        if let Some(content) = attr(tag, tag_lower, "content") {
            let key = attr(tag, tag_lower, "property")
                .or_else(|| attr(tag, tag_lower, "name"))
                .unwrap_or_default()
                .to_ascii_lowercase();
            match key.as_str() {
                "og:title" if og_title.is_empty() => og_title = decode_entities(&content),
                "twitter:title" if tw_title.is_empty() => tw_title = decode_entities(&content),
                "og:site_name" if og_site.is_empty() => og_site = decode_entities(&content),
                "og:description" if og_desc.is_empty() => og_desc = decode_entities(&content),
                "description" | "twitter:description" if meta_desc.is_empty() => {
                    meta_desc = decode_entities(&content)
                }
                _ => {}
            }
        }
        pos = end;
    }

    let mut title = if !og_title.is_empty() {
        og_title
    } else if !tw_title.is_empty() {
        tw_title
    } else {
        extract_title_tag(html, &lower).unwrap_or_default()
    };
    let mut description = if !og_desc.is_empty() { og_desc } else { meta_desc };
    let mut site = if !og_site.is_empty() {
        og_site
    } else {
        host_of(url).to_owned()
    };

    title = title.trim().to_owned();
    description = description.trim().to_owned();
    site = site.trim().to_owned();
    if title.is_empty() && description.is_empty() {
        return None;
    }
    protocol::clip(&mut title, 200);
    protocol::clip(&mut description, 400);
    protocol::clip(&mut site, 100);
    Some(Preview {
        url: url.to_owned(),
        site,
        title,
        description,
        image: None,
    })
}

fn extract_title_tag(html: &str, lower: &str) -> Option<String> {
    let start = lower.find("<title")?;
    let open_end = lower[start..].find('>')? + start + 1;
    let close = lower[open_end..].find("</title>")? + open_end;
    let raw = html[open_end..close].trim();
    (!raw.is_empty()).then(|| decode_entities(raw))
}

fn attr(tag: &str, tag_lower: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let mut search_from = 0;
    while let Some(rel) = tag_lower[search_from..].find(&needle) {
        let at = search_from + rel;
        let before_ok = at == 0 || tag_lower.as_bytes()[at - 1].is_ascii_whitespace();
        search_from = at + needle.len();
        if !before_ok {
            continue;
        }
        let rest = &tag[search_from..];
        let mut chars = rest.chars();
        let quote = chars.next()?;
        if quote == '"' || quote == '\'' {
            let inner = &rest[quote.len_utf8()..];
            let end = inner.find(quote)?;
            return Some(inner[..end].to_owned());
        }
        let end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        return Some(rest[..end].to_owned());
    }
    None
}

fn host_of(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let authority = rest.split('/').next().unwrap_or(rest);
    authority.split(':').next().unwrap_or(authority)
}

fn decode_entities(raw: &str) -> String {
    raw.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn extracts_first_url_and_trims_punctuation() {
        assert_eq!(
            extract_url("Check out https://example.com/post?a=1.").as_deref(),
            Some("https://example.com/post?a=1")
        );
        assert_eq!(
            extract_url("See (http://127.0.0.1:8080/hello)!").as_deref(),
            Some("http://127.0.0.1:8080/hello")
        );
        assert_eq!(extract_url("No links here"), None);
    }

    #[test]
    fn unfurls_local_http_stub_and_caches_the_result() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_bg = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                hits_bg.fetch_add(1, Ordering::SeqCst);
                let mut stream = stream;
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let body = r#"<!doctype html><html><head>
                    <meta property="og:title" content="Tiny &amp; Fast">
                    <meta property="og:site_name" content="Example News">
                    <meta property="og:description" content="A short summary.">
                </head><body>ok</body></html>"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let cache = Mutex::new(HashMap::new());
        let url = format!("http://{addr}/article");
        let first = fetch_with_cache(&url, &cache).expect("preview");
        assert_eq!(first.title, "Tiny & Fast");
        assert_eq!(first.site, "Example News");
        assert_eq!(first.description, "A short summary.");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Second call hits the in-memory cache without contacting the server again.
        let second = fetch_with_cache(&url, &cache).expect("cached preview");
        assert_eq!(second, first);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
