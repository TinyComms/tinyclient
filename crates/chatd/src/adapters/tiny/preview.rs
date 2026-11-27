//! Sender-side link preview fetcher for Tiny servers (#308).
//!
//! Only the sender's device fetches the linked page (when link previews are enabled), builds a
//! small card, and sends it with the message (plaintext `previews` or inside the sealed E2EE
//! envelope). Readers never contact the linked site.
//!
//! Hardened against SSRF and hostile pages:
//! - `https://` only; no userinfo/credentials in the URL.
//! - Resolves host via DNS and refuses any non-public IP (private, loopback, link-local,
//!   unspecified, multicast, CGNAT, ULA, IPv4-mapped non-public IPv6) BEFORE connect and on
//!   EVERY redirect hop.
//! - Manual redirect loop: at most 3 redirects, 5 second total deadline.
//! - No cookies, no `Referer`, fixed generic User-Agent.
//! - Reads at most 512 KiB of HTML and at most 2 MiB of image bytes.
//! - Decodes the image with `image` crate limits, resizes long edge <= 640px, and re-encodes
//!   as JPEG — never forwarding original bytes.

use std::io::{Cursor, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::time::{Duration, Instant};

const MAX_REDIRECTS: usize = 3;
const TOTAL_DEADLINE: Duration = Duration::from_millis(1500);
const PREVIEW_IDLE: Duration = Duration::from_secs(30);
const MAX_HTML_BYTES: u64 = 512 * 1024;
const MAX_IMAGE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_IMAGE_EDGE: u32 = 640;
const JPEG_QUALITY: u8 = 82;
const USER_AGENT: &str = "Mozilla/5.0 (compatible; LinkPreview/1.0)";

fn preview_agent() -> ureq::Agent {
    static SHARED: std::sync::Mutex<Option<(protocol::settings::ProxyConfig, ureq::Agent)>> =
        std::sync::Mutex::new(None);
    let cfg = crate::adapters::net::proxy_config();
    let mut guard = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((saved, agent)) = guard.as_ref() {
        if *saved == cfg {
            return agent.clone();
        }
    }
    let built: ureq::Agent = crate::adapters::net::apply_proxy(ureq::Agent::config_builder())
        .tls_config(crate::tls::config())
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .max_idle_age(PREVIEW_IDLE)
        .timeout_global(Some(TOTAL_DEADLINE))
        .user_agent(USER_AGENT)
        .build()
        .into();
    *guard = Some((cfg, built.clone()));
    built
}

const MAX_SITE_LEN: usize = 100;
const MAX_TITLE_LEN: usize = 200;
const MAX_DESC_LEN: usize = 500;

/// A fetched link preview card ready to upload/send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FetchedPreview {
    pub url: String,
    pub site: String,
    pub title: String,
    pub description: String,
    pub image: Option<FetchedImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FetchedImage {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedUrl {
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Finds the first `https://` URL in `text`, trimming trailing sentence punctuation.
pub(crate) fn first_https_url(text: &str) -> Option<String> {
    let start = text.find("https://")?;
    let rest = &text[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let mut candidate = &rest[..end];
    while let Some(last) = candidate.chars().last() {
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '>' | '"' | '\'') {
            candidate = &candidate[..candidate.len() - last.len_utf8()];
        } else {
            break;
        }
    }
    validate_https_url(candidate).ok()?;
    Some(candidate.to_owned())
}

/// Extracts the real display host from an `http://` or `https://` URL. Always used on receive so
/// a spoofed `site` field cannot misrepresent where a link actually goes.
pub(crate) fn real_host(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let authority = rest.split(&['/', '?', '#'][..]).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed
            .split(']')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    } else {
        host_port
            .split(':')
            .next()
            .unwrap_or("")
            .trim_end_matches('.')
            .to_ascii_lowercase()
    }
}

/// Validates an `https://` URL: refuses non-https, userinfo/credentials, control characters,
/// whitespace, or empty host.
pub(crate) fn validate_https_url(url: &str) -> Result<ParsedUrl, &'static str> {
    if url.len() > 2048 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("invalid url");
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return Err("https required");
    };
    let (authority, path) = match rest.find(&['/', '?', '#'][..]) {
        Some(ix) => (&rest[..ix], &rest[ix..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') {
        return Err("credentials not allowed");
    }
    let (host, port) = if let Some(after_open) = authority.strip_prefix('[') {
        let Some(close) = after_open.find(']') else {
            return Err("invalid ipv6 host");
        };
        let host = &after_open[..close];
        let tail = &after_open[close + 1..];
        let port = if tail.is_empty() {
            443
        } else if let Some(p) = tail.strip_prefix(':') {
            p.parse::<u16>().map_err(|_| "invalid port")?
        } else {
            return Err("invalid authority");
        };
        (host.to_ascii_lowercase(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_ascii_lowercase(), p.parse::<u16>().map_err(|_| "invalid port")?),
            None => (authority.to_ascii_lowercase(), 443),
        }
    };
    if host.is_empty() || port == 0 {
        return Err("empty host");
    }
    Ok(ParsedUrl { host, port, path: path.to_owned() })
}

/// True only for globally routable public IP addresses.
pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
    {
        return false;
    }
    // 0.0.0.0/8 ("this network")
    if a == 0 {
        return false;
    }
    // 100.64.0.0/10 (CGNAT / Tailscale)
    if a == 100 && (64..=127).contains(&b) {
        return false;
    }
    // 192.0.0.0/24 (IETF protocol assignments)
    if a == 192 && b == 0 && c == 0 {
        return false;
    }
    // 198.18.0.0/15 (benchmarking)
    if a == 198 && (b == 18 || b == 19) {
        return false;
    }
    // 240.0.0.0/4 (reserved)
    if a >= 240 {
        return false;
    }
    true
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let seg0 = ip.segments()[0];
    // Unique local fc00::/7
    if (seg0 & 0xfe00) == 0xfc00 {
        return false;
    }
    // Link-local fe80::/10
    if (seg0 & 0xffc0) == 0xfe80 {
        return false;
    }
    // Site-local (deprecated) fec0::/10
    if (seg0 & 0xffc0) == 0xfec0 {
        return false;
    }
    // Documentation 2001:db8::/32
    if seg0 == 0x2001 && ip.segments()[1] == 0x0db8 {
        return false;
    }
    true
}

/// Resolves `(host, port)` on a helper thread bounded by `timeout` so a hung DNS resolver
/// cannot stall sending beyond the preview budget.
fn resolve_addrs_bounded(
    host: String,
    port: u16,
    timeout: Duration,
) -> Result<Vec<std::net::SocketAddr>, &'static str> {
    if timeout.is_zero() {
        return Err("timeout");
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("tiny-preview-dns".into())
        .spawn(move || {
            let res = (host.as_str(), port)
                .to_socket_addrs()
                .map(|iter| iter.collect::<Vec<_>>());
            let _ = tx.send(res);
        })
        .map_err(|_| "dns failed")?;
    match rx.recv_timeout(timeout) {
        Ok(Ok(addrs)) => Ok(addrs),
        Ok(Err(_)) => Err("dns failed"),
        Err(_) => Err("timeout"),
    }
}

/// Validates a URL and checks that EVERY resolved IP address is public.
#[cfg(test)]
pub(crate) fn check_public_target(url: &str) -> Result<ParsedUrl, &'static str> {
    check_public_target_within(url, Instant::now() + TOTAL_DEADLINE)
}

fn check_public_target_within(url: &str, deadline: Instant) -> Result<ParsedUrl, &'static str> {
    let parsed = validate_https_url(url)?;
    if parsed.host == "localhost" || parsed.host.ends_with(".localhost") {
        return Err("local host");
    }
    if let Ok(ip) = parsed.host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err("non-public ip");
        }
        return Ok(parsed);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let addrs = resolve_addrs_bounded(parsed.host.clone(), parsed.port, remaining)?;
    let mut count = 0;
    for addr in addrs {
        count += 1;
        if !is_public_ip(addr.ip()) {
            return Err("non-public ip");
        }
    }
    if count == 0 {
        return Err("no addresses");
    }
    Ok(parsed)
}

/// Fetches an HTTPS URL with manual SSRF-checked redirects, up to `max_bytes`, within `deadline`.
/// Returns `(final_url, content_type, body_bytes)`.
fn fetch_checked(
    start_url: &str,
    max_bytes: u64,
    deadline: Instant,
) -> Result<(String, String, Vec<u8>), &'static str> {
    let agent = preview_agent();
    let mut current = start_url.to_owned();
    for hop in 0..=MAX_REDIRECTS {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("timeout");
        }
        let _ = check_public_target_within(&current, deadline)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("timeout");
        }
        let response = agent
            .get(&current)
            .config()
            .timeout_global(Some(remaining))
            .build()
            .header("Accept", "text/html,application/xhtml+xml,image/*;q=0.8,*/*;q=0.1")
            .call()
            .map_err(|_| "request failed")?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if hop == MAX_REDIRECTS {
                return Err("too many redirects");
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or("missing redirect location")?;
            current = resolve_redirect(&current, location).ok_or("invalid redirect")?;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err("non-2xx status");
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut reader = response.into_body().into_reader().take(max_bytes);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).map_err(|_| "read failed")?;
        return Ok((current, content_type, buf));
    }
    Err("too many redirects")
}

/// Resolves a redirect `Location` or `og:image` reference against `base_url`.
pub(crate) fn resolve_redirect(base_url: &str, target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    if target.starts_with("https://") {
        return Some(target.to_owned());
    }
    if target.starts_with("http://") {
        return None;
    }
    if let Some(rest) = target.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    let parsed = validate_https_url(base_url).ok()?;
    let origin = if parsed.port == 443 {
        format!("https://{}", parsed.host)
    } else {
        format!("https://{}:{}", parsed.host, parsed.port)
    };
    if target.starts_with('/') {
        Some(format!("{origin}{target}"))
    } else {
        let base_path = parsed.path.split(&['?', '#'][..]).next().unwrap_or("/");
        let dir = match base_path.rfind('/') {
            Some(ix) => &base_path[..=ix],
            None => "/",
        };
        Some(format!("{origin}{dir}{target}"))
    }
}

/// Fetches `url`, parses OpenGraph / HTML title metadata, and optionally fetches + re-encodes
/// `og:image` as a <=640px JPEG. Returns `None` if blocked, unreachable, or has no metadata.
pub(crate) fn fetch_preview(url: &str) -> Option<FetchedPreview> {
    let deadline = Instant::now() + TOTAL_DEADLINE;
    let (final_url, content_type, bytes) = fetch_checked(url, MAX_HTML_BYTES, deadline).ok()?;
    if !content_type.is_empty()
        && !content_type.contains("text/html")
        && !content_type.contains("application/xhtml")
    {
        return None;
    }
    let html = String::from_utf8_lossy(&bytes);
    let mut parsed = parse_html_metadata(&html, url)?;
    if let Some(image_ref) = parsed.image_url.take() {
        if let Some(resolved) = resolve_redirect(&final_url, &image_ref) {
            if let Ok((_, _, img_bytes)) = fetch_checked(&resolved, MAX_IMAGE_BYTES, deadline) {
                if let Some(image) = reencode_preview_image(&img_bytes) {
                    return Some(FetchedPreview {
                        url: url.to_owned(),
                        site: parsed.site,
                        title: parsed.title,
                        description: parsed.description,
                        image: Some(image),
                    });
                }
            }
        }
    }
    if parsed.title.is_empty() && parsed.description.is_empty() {
        return None;
    }
    Some(FetchedPreview {
        url: url.to_owned(),
        site: parsed.site,
        title: parsed.title,
        description: parsed.description,
        image: None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedMetadata {
    pub site: String,
    pub title: String,
    pub description: String,
    pub image_url: Option<String>,
}

/// Parses OpenGraph / HTML metadata from an HTML document.
pub(crate) fn parse_html_metadata(html: &str, url: &str) -> Option<ParsedMetadata> {
    let mut og_title = None;
    let mut og_desc = None;
    let mut og_site = None;
    let mut og_image = None;
    let mut meta_desc = None;

    let mut pos = 0;
    let bytes = html.as_bytes();
    while pos < bytes.len() {
        let Some(lt_rel) = html[pos..].find('<') else {
            break;
        };
        let lt = pos + lt_rel;
        let Some(gt_rel) = html[lt..].find('>') else {
            break;
        };
        let gt = lt + gt_rel;
        let tag = &html[lt + 1..gt];
        pos = gt + 1;

        let trimmed = tag.trim_start();
        if trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("meta") {
            let after = trimmed.as_bytes().get(4).copied().unwrap_or(b' ');
            if !after.is_ascii_whitespace() && after != b'/' {
                continue;
            }
            let prop = html_attr(trimmed, "property")
                .or_else(|| html_attr(trimmed, "name"))
                .map(|s| s.to_ascii_lowercase());
            let content = html_attr(trimmed, "content");
            if let (Some(prop), Some(content)) = (prop, content) {
                let decoded = clean_field(&decode_entities(&content), MAX_DESC_LEN);
                if decoded.is_empty() {
                    continue;
                }
                match prop.as_str() {
                    "og:title" | "twitter:title" if og_title.is_none() => og_title = Some(decoded),
                    "og:description" | "twitter:description" if og_desc.is_none() => og_desc = Some(decoded),
                    "description" if meta_desc.is_none() => meta_desc = Some(decoded),
                    "og:site_name" if og_site.is_none() => og_site = Some(decoded),
                    "og:image" | "twitter:image" if og_image.is_none() => og_image = Some(content.trim().to_owned()),
                    _ => {}
                }
            }
        }
    }

    let title_fallback = extract_title(html);
    let title = clean_field(&og_title.or(title_fallback).unwrap_or_default(), MAX_TITLE_LEN);
    let description = clean_field(&og_desc.or(meta_desc).unwrap_or_default(), MAX_DESC_LEN);
    let site = clean_field(
        &og_site.unwrap_or_else(|| real_host(url)),
        MAX_SITE_LEN,
    );
    if title.is_empty() && description.is_empty() && og_image.is_none() {
        return None;
    }
    Some(ParsedMetadata {
        site,
        title,
        description,
        image_url: og_image,
    })
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let start_rel = lower[open..].find('>')?;
    let content_start = open + start_rel + 1;
    let close_rel = lower[content_start..].find("</title>")?;
    let raw = &html[content_start..content_start + close_rel];
    let decoded = clean_field(&decode_entities(raw), MAX_TITLE_LEN);
    (!decoded.is_empty()).then_some(decoded)
}

fn html_attr(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' {
            i += 1;
        }
        let key = &tag[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let value = if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            i += 1;
            let val_start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            let v = &tag[val_start..i];
            if i < bytes.len() {
                i += 1;
            }
            v
        } else {
            let val_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' {
                i += 1;
            }
            &tag[val_start..i]
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value.to_owned());
        }
    }
    None
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';').filter(|&ix| ix <= 12) {
            let entity = &after[..semi];
            let decoded = match entity {
                "amp" => Some("&".to_owned()),
                "lt" => Some("<".to_owned()),
                "gt" => Some(">".to_owned()),
                "quot" => Some("\"".to_owned()),
                "apos" | "#39" => Some("'".to_owned()),
                "nbsp" => Some(" ".to_owned()),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32).map(|c| c.to_string())
                }
                _ if entity.starts_with('#') => {
                    entity[1..].parse::<u32>().ok().and_then(char::from_u32).map(|c| c.to_string())
                }
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push_str(&ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Strips control characters, collapses whitespace, and clips to `max_chars`.
pub(crate) fn clean_field(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut prev_space = true;
    for ch in text.chars() {
        if ch.is_control() {
            if !prev_space && (ch == '\n' || ch == '\r' || ch == '\t') {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    let trimmed = out.trim();
    trimmed.chars().take(max_chars).collect()
}

/// Decodes raw image bytes with strict memory limits, resizes long edge <= 640px, and re-encodes
/// as JPEG (never forwarding original bytes).
pub(crate) fn reencode_preview_image(raw: &[u8]) -> Option<FetchedImage> {
    if raw.is_empty() || raw.len() as u64 > MAX_IMAGE_BYTES {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(raw))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut img = reader.decode().ok()?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    if w > MAX_IMAGE_EDGE || h > MAX_IMAGE_EDGE {
        img = img.resize(MAX_IMAGE_EDGE, MAX_IMAGE_EDGE, image::imageops::FilterType::Triangle);
    }
    let (out_w, out_h) = (img.width(), img.height());
    let rgb = img.to_rgb8();
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);
    rgb.write_with_encoder(encoder).ok()?;
    Some(FetchedImage {
        jpeg: buf.into_inner(),
        width: out_w,
        height: out_h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_http_credentials_and_non_public_ips() {
        assert!(validate_https_url("http://example.com").is_err());
        assert!(validate_https_url("https://user:pass@example.com/a").is_err());
        assert!(validate_https_url("https://example.com/\nfoo").is_err());
        assert!(validate_https_url("https://example.com/path").is_ok());

        for blocked in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.5.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.0.1",
        ] {
            let ip: IpAddr = blocked.parse().unwrap();
            assert!(!is_public_ip(ip), "{blocked} must be refused");
        }
        for allowed in ["93.184.216.34", "1.1.1.1", "2606:2800:220:1:248:1893:25c8:1946"] {
            let ip: IpAddr = allowed.parse().unwrap();
            assert!(is_public_ip(ip), "{allowed} must be allowed");
        }

        assert!(check_public_target("https://127.0.0.1/secret").is_err());
        assert!(check_public_target("https://169.254.169.254/latest/meta-data").is_err());
        assert!(check_public_target("https://localhost/admin").is_err());
    }

    #[test]
    fn redirect_resolution_blocks_downgrade_and_resolves_relative() {
        assert_eq!(
            resolve_redirect("https://example.com/a/b", "/c"),
            Some("https://example.com/c".to_owned())
        );
        assert_eq!(
            resolve_redirect("https://example.com/a/b", "img.png"),
            Some("https://example.com/a/img.png".to_owned())
        );
        assert_eq!(resolve_redirect("https://example.com/a", "http://example.com/b"), None);
        let to_private = resolve_redirect("https://example.com/a", "https://127.0.0.1/secret").unwrap();
        assert!(check_public_target(&to_private).is_err());
    }

    #[test]
    fn parses_opengraph_title_fallback_entities_and_no_metadata() {
        let html = r#"<!doctype html><html><head>
            <meta property="og:title" content="Hello &amp; Welcome">
            <meta property="og:description" content="A &quot;tiny&quot; page&#39;s summary">
            <meta property="og:site_name" content="Spoofed Site">
            <meta property="og:image" content="/thumb.png">
        </head><body></body></html>"#;
        let meta = parse_html_metadata(html, "https://real.example.org/post").unwrap();
        assert_eq!(meta.title, "Hello & Welcome");
        assert_eq!(meta.description, "A \"tiny\" page's summary");
        assert_eq!(meta.site, "Spoofed Site");
        assert_eq!(meta.image_url.as_deref(), Some("/thumb.png"));

        let fallback = parse_html_metadata("<html><head><title>Plain Title</title></head></html>", "https://example.org").unwrap();
        assert_eq!(fallback.title, "Plain Title");
        assert_eq!(fallback.site, "example.org");

        assert!(parse_html_metadata("<html><body>no metadata here</body></html>", "https://example.org").is_none());
    }

    #[test]
    fn real_host_and_first_https_url_behave_safely() {
        assert_eq!(real_host("https://Sub.Example.COM:8443/path?x=1"), "sub.example.com");
        assert_eq!(real_host("https://evil@real.example.com/x"), "real.example.com");
        assert_eq!(
            first_https_url("Check out https://example.com/article, it's great!"),
            Some("https://example.com/article".to_owned())
        );
        assert_eq!(first_https_url("Plain http://example.com is ignored"), None);
    }

    #[test]
    fn reencode_preview_image_resizes_and_outputs_jpeg() {
        let img = image::RgbaImage::from_pixel(1280, 640, image::Rgba([20, 120, 220, 255]));
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let out = reencode_preview_image(&png.into_inner()).expect("reencoded");
        assert_eq!(out.width, 640);
        assert_eq!(out.height, 320);
        assert!(out.jpeg.starts_with(&[0xFF, 0xD8]), "output must be JPEG");
    }

    #[test]
    fn preview_respects_total_deadline() {
        assert!(
            TOTAL_DEADLINE <= Duration::from_millis(1500),
            "preview total budget should stay bounded at <= 1500ms, got {TOTAL_DEADLINE:?}"
        );
        let started = Instant::now();
        assert_eq!(
            resolve_addrs_bounded("example.com".to_owned(), 443, Duration::ZERO),
            Err("timeout")
        );
        assert_eq!(
            check_public_target_within("https://example.com/", Instant::now()),
            Err("timeout")
        );
        assert_eq!(
            fetch_checked("https://1.1.1.1/", MAX_HTML_BYTES, Instant::now()),
            Err("timeout")
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "expired preview deadline must fail promptly"
        );
    }
}
