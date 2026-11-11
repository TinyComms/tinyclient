//! Web links in message text, and what a click on one should tell the person first.
//!
//! Two halves, both pure:
//!
//! * [`find`] and [`linkify`] turn the `http://`, `https://` and `www.` addresses a person typed
//!   into [`Style::Link`] spans. Only the web: no `mailto:`, `file:`, `javascript:`, `intent:`
//!   or custom scheme is ever a link, and bare domains (`example.com`) are not guessed at,
//!   because a file name or a sentence without a space after its full stop would be a false link.
//!   The text is never changed and the address is kept as it was typed (percent-encoding and
//!   unicode included); a `www.` address gets `https://` in front of it in the span only.
//! * [`Destination`] reads an address the way the person should: the real host, the registrable
//!   domain, the host in its `xn--` and its unicode form, and whether it looks like a homograph.
//!   It also strips tracking parameters, and holds the [`Trusted`] list of domains a device was
//!   told not to ask about.
//!
//! No wire format changes: spans are local rendering of text that is already there, so end-to-end
//! encrypted messages and older clients are unaffected.

use crate::{Message, MessageKind, Span, Style};
use std::collections::BTreeSet;
use std::ops::Range;
use url::{Host, Url};

/// The longest address that becomes a link, in bytes. The same ceiling the browser opener has.
pub const LINK_LIMIT: usize = 8 * 1024;
/// Most links one message gets. Further addresses stay text.
pub const LINKS_PER_MESSAGE: usize = 64;

/// One address found in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Where it is in the text, in bytes, on character boundaries.
    pub range: Range<usize>,
    /// What it opens: the text as typed, with `https://` added in front of a bare `www.` address.
    pub url: String,
}

/// Whether `url` is a web address and nothing a shell or a handler could take as something else.
pub fn is_web(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && url.len() <= LINK_LIMIT
        && !url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

/// The first link a message has, in the order it reads, for a later link card.
pub fn first_link(text: &str, spans: &[Span]) -> Option<String> {
    let mut links: Vec<(u32, &str)> = spans
        .iter()
        .filter_map(|span| match &span.style {
            Style::Link { url } if is_web(url) && (span.start as usize) <= text.len() => Some((span.start, url.as_str())),
            _ => None,
        })
        .collect();
    links.sort_by_key(|(start, _)| *start);
    links.first().map(|(_, url)| (*url).to_owned())
}

/// Gives a message's web addresses a [`Style::Link`] span each. Idempotent: an address that
/// already has a link span is left alone. Does nothing for a system line or a deleted message,
/// and nothing inside a code span or block (the ones the message already carries, and a pair
/// of backticks in the text).
pub fn linkify(message: &mut Message) {
    if message.deleted || message.text.is_empty() || message.kind != MessageKind::Normal {
        return;
    }
    let mut skip = code_ranges(&message.text);
    for span in &message.spans {
        if matches!(span.style, Style::Code | Style::CodeBlock { .. } | Style::Link { .. }) {
            skip.push(span.start as usize..span.end as usize);
        }
    }
    let found = find(&message.text, &skip);
    for one in found {
        message.spans.push(Span { start: one.range.start as u32, end: one.range.end as u32, style: Style::Link { url: one.url } });
    }
}

/// Backtick code in the text: ```` ```fenced``` ```` across lines, and `` `inline` `` on one line.
/// An opening mark with no closing one is just a character.
fn code_ranges(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find('`') {
        let open = at + found;
        if text[open..].starts_with("```") {
            match text[open + 3..].find("```") {
                Some(close) => {
                    let end = open + 3 + close + 3;
                    out.push(open..end);
                    at = end;
                }
                None => at = open + 3,
            }
        } else {
            let line_end = text[open + 1..].find('\n').map_or(text.len(), |n| open + 1 + n);
            match text[open + 1..line_end].find('`') {
                Some(close) => {
                    let end = open + 1 + close + 1;
                    out.push(open..end);
                    at = end;
                }
                None => at = open + 1,
            }
        }
    }
    out
}

/// A character that ends an address: space, a control or invisible character (a zero-width or
/// bidirectional mark is how an address is made to read as another), a quote or an angle bracket
/// (`<https://…>`), a backtick, a backslash, and the full-width punctuation that follows an
/// address in Chinese and Japanese text.
fn ends_address(c: char) -> bool {
    if c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '`' | '\\') {
        return true;
    }
    matches!(c as u32,
        0x00AD | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x2069 | 0xFEFF
        | 0x3000..=0x3003 | 0x3008..=0x3011 | 0x3014..=0x301F
        | 0xFF01..=0xFF0F | 0xFF1A..=0xFF20 | 0xFF3B..=0xFF40 | 0xFF5B..=0xFF65)
}

/// Whether an address may start right after `before`: not in the middle of a word, an e-mail
/// address, or another address's path.
fn starts_here(before: Option<char>) -> bool {
    !before.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '.' | '-' | '/' | '\\' | '%' | '+'))
}

/// Cuts the punctuation of the sentence off the end of an address, and a closing bracket that
/// has no opening one inside it (so `(see https://a.example/x)` loses its `)` and
/// `https://a.example/Rust_(language)` keeps it).
fn trimmed(address: &str) -> &str {
    let mut end = address.len();
    while let Some(last) = address[..end].chars().next_back() {
        let kept = &address[..end];
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '\u{2026}' | '\u{3002}' => true,
            ')' => kept.matches(')').count() > kept.matches('(').count(),
            ']' => kept.matches(']').count() > kept.matches('[').count(),
            '}' => kept.matches('}').count() > kept.matches('{').count(),
            _ => false,
        };
        if !drop {
            break;
        }
        end -= last.len_utf8();
    }
    &address[..end]
}

fn is_tld(label: &str) -> bool {
    (label.len() >= 2 && label.chars().all(|c| c.is_alphabetic())) || (label.starts_with("xn--") && label.len() > 4)
}

/// Parses an address a person typed. `None` when it is not a web address with a host.
fn parse_web(address: &str) -> Option<Url> {
    let url = Url::parse(address).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    match url.host()? {
        Host::Domain(domain) if domain.is_empty() || domain.starts_with('.') || domain.contains("..") => None,
        _ => Some(url),
    }
}

/// Every web address in `text`, in order. `skip` is ranges of the text where none starts and
/// none reaches (code).
pub fn find(text: &str, skip: &[Range<usize>]) -> Vec<Found> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() && out.len() < LINKS_PER_MESSAGE {
        if !matches!(bytes[i], b'h' | b'H' | b'w' | b'W') || !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        let scheme = if rest.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")) {
            8
        } else if rest.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://")) {
            7
        } else {
            0
        };
        let bare = scheme == 0 && rest.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("www."));
        if (scheme == 0 && !bare) || !starts_here(text[..i].chars().next_back()) {
            i += 1;
            continue;
        }
        let mut token = rest.find(ends_address).unwrap_or(rest.len());
        // `[https://a.example](https://a.example)`: the label ends where the markdown link begins.
        if let Some(cut) = rest[..token].find("](") {
            token = cut;
        }
        if token > LINK_LIMIT {
            i += token;
            continue;
        }
        let address = trimmed(&rest[..token]);
        let range = i..i + address.len();
        let blocked = skip.iter().any(|code| range.start < code.end && code.start < range.end);
        let url = if blocked || address.len() <= scheme.max(4) {
            None
        } else if bare {
            let full = format!("https://{address}");
            parse_web(&full).filter(|url| {
                let host = url.host_str().unwrap_or("");
                let labels: Vec<&str> = host.split('.').collect();
                labels.len() >= 3 && labels.iter().all(|label| !label.is_empty()) && is_tld(labels[labels.len() - 1])
            }).map(|_| full)
        } else {
            parse_web(address).map(|_| address.to_owned())
        };
        match url {
            Some(url) => {
                out.push(Found { range: range.clone(), url });
                i = range.end.max(i + 1);
            }
            None => i += 1,
        }
    }
    out
}

// What a click should say.

/// How strange a host looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    /// Plain ASCII.
    None,
    /// Non-ASCII letters in the name (an internationalised domain). Legitimate, and worth showing in
    /// both forms.
    Idn,
    /// Mixed scripts in one label, or letters from another script that pass for Latin ones: the
    /// shape of a homograph attack.
    Lookalike,
}

/// Where a link goes, as the person should read it before it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// The address as the browser will be given it: parsed and normalised, with percent-encoding
    /// left as it was typed.
    pub url: String,
    /// The host in ASCII, `xn--` labels as sent over the wire.
    pub host: String,
    /// The same host with punycode decoded, for display.
    pub host_shown: String,
    /// The registrable domain (`example.co.uk` of `www.example.co.uk`), ASCII. Approximate: a
    /// short list of two-part suffixes and of hosts that give a name to everyone.
    pub domain: String,
    /// The same, decoded.
    pub domain_shown: String,
    pub risk: Risk,
    /// The address has a user name in it (`https://paypal.com@evil.example/`), which people
    /// read as the host.
    pub has_userinfo: bool,
}

impl Destination {
    /// `None` for anything but an http or https address with a host.
    pub fn parse(raw: &str) -> Option<Destination> {
        if !is_web(raw) {
            return None;
        }
        let url = parse_web(raw)?;
        let host = url.host_str()?.to_ascii_lowercase();
        let (shown, decoded) = idna::domain_to_unicode(&host);
        let domain = registrable(&host);
        let (domain_shown, _) = idna::domain_to_unicode(&domain);
        // An `xn--` label that does not decode is not a name anyone typed.
        let risk = if decoded.is_err() { Risk::Lookalike } else { risk_of(&shown) };
        let has_userinfo = !url.username().is_empty() || url.password().is_some();
        Some(Destination { url: url.into(), host, host_shown: shown, domain, domain_shown, risk, has_userinfo })
    }

    /// What to hand the browser. With `strip`, tracking parameters are taken out first.
    pub fn target(&self, strip: bool) -> String {
        if strip {
            strip_tracking(&self.url)
        } else {
            self.url.clone()
        }
    }
}

/// Hosts where every name under them belongs to someone else, so trusting the registrable
/// domain would trust everyone.
const SHARED_HOSTS: &[&str] = &[
    "github.io", "gitlab.io", "blogspot.com", "wordpress.com", "vercel.app", "netlify.app", "pages.dev", "workers.dev",
    "herokuapp.com", "web.app", "firebaseapp.com", "appspot.com", "azurewebsites.net", "cloudfront.net", "s3.amazonaws.com",
    "amazonaws.com", "myshopify.com", "wixsite.com", "weebly.com", "substack.com", "notion.site", "onrender.com", "fly.dev",
    "repl.co", "glitch.me", "ngrok.io", "ngrok-free.app", "trycloudflare.com", "duckdns.org", "tumblr.com",
];

/// The second-level labels that sit under a country code (`co.uk`, `com.au`).
const SECOND_LEVEL: &[&str] = &["co", "com", "org", "net", "gov", "edu", "ac", "ne", "or", "go", "gob", "nom", "mil", "sch"];

/// The registrable domain of an ASCII host. An IP address, or a name with one label, is itself.
pub fn registrable(host: &str) -> String {
    let host = host.trim_end_matches('.');
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return host.to_owned();
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() <= 2 {
        return host.to_owned();
    }
    // A host that hands out names: one label more than the host's own suffix.
    let shared = SHARED_HOSTS.iter().find(|shared| host == **shared || host.ends_with(&format!(".{shared}")));
    let keep = if let Some(shared) = shared {
        shared.split('.').count() + 1
    } else if labels[labels.len() - 1].len() == 2 && SECOND_LEVEL.contains(&labels[labels.len() - 2]) {
        3
    } else {
        2
    };
    labels[labels.len().saturating_sub(keep)..].join(".")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
    Greek,
    Armenian,
    Cherokee,
    Han,
    Hiragana,
    Katakana,
    Hangul,
    Other,
    /// Digits and the hyphen: they belong to every script.
    Common,
}

fn script(c: char) -> Script {
    if c.is_ascii_digit() || c == '-' {
        return Script::Common;
    }
    match c as u32 {
        0x41..=0x5A | 0x61..=0x7A | 0xAA | 0xBA | 0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x24F | 0x1E00..=0x1EFF | 0x2C60..=0x2C7F | 0xA720..=0xA7FF => Script::Latin,
        0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
        0x0400..=0x052F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => Script::Cyrillic,
        0x0530..=0x058F => Script::Armenian,
        0x13A0..=0x13FF => Script::Cherokee,
        0x3040..=0x309F => Script::Hiragana,
        0x30A0..=0x30FF | 0x31F0..=0x31FF => Script::Katakana,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => Script::Hangul,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2A6DF => Script::Han,
        _ => Script::Other,
    }
}

/// Letters outside Latin that are drawn like a Latin one in common fonts.
fn passes_for_latin(c: char) -> bool {
    matches!(c,
        '\u{0430}' | '\u{0435}' | '\u{043E}' | '\u{0440}' | '\u{0441}' | '\u{0443}' | '\u{0445}' | '\u{0456}' | '\u{0458}'
        | '\u{0455}' | '\u{04BB}' | '\u{0501}' | '\u{051B}' | '\u{051D}' | '\u{04CF}' | '\u{0432}' | '\u{043A}' | '\u{043D}' | '\u{0442}'
        | '\u{03BF}' | '\u{03BD}' | '\u{03B1}' | '\u{03C1}' | '\u{03B9}' | '\u{03BA}' | '\u{03C4}' | '\u{03C5}' | '\u{03C7}' | '\u{03B5}'
        | '\u{0570}' | '\u{0578}' | '\u{057D}' | '\u{0581}' | '\u{0585}' | '\u{0131}' | '\u{0237}' | '\u{1D00}'..='\u{1D2B}')
}

fn risk_of(shown: &str) -> Risk {
    if shown.is_ascii() {
        return Risk::None;
    }
    for label in shown.split('.') {
        let mut scripts: Vec<Script> = label.chars().map(script).filter(|s| *s != Script::Common).collect();
        scripts.sort_by_key(|s| *s as u8);
        scripts.dedup();
        // Japanese and Korean names mix Han with their own letters as a matter of course.
        let japanese = scripts.iter().all(|s| matches!(s, Script::Han | Script::Hiragana | Script::Katakana));
        let korean = scripts.iter().all(|s| matches!(s, Script::Han | Script::Hangul));
        if scripts.len() > 1 && !japanese && !korean {
            return Risk::Lookalike;
        }
        // Every letter is Latin or passes for one, and some is not Latin.
        let foreign = !label.is_ascii();
        let looks_latin = label.chars().all(|c| c.is_ascii() || passes_for_latin(c) || script(c) == Script::Common);
        if foreign && looks_latin && label.chars().any(passes_for_latin) {
            return Risk::Lookalike;
        }
    }
    Risk::Idn
}

/// Query parameters that only follow a click around.
fn is_tracker(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("utm_")
        || name.starts_with("pk_")
        || name.starts_with("mtm_")
        || matches!(name.as_str(),
            "fbclid" | "gclid" | "gclsrc" | "dclid" | "gbraid" | "wbraid" | "msclkid" | "mc_eid" | "mc_cid" | "igshid" | "igsh"
            | "yclid" | "twclid" | "ttclid" | "li_fat_id" | "_hsenc" | "_hsmi" | "__hssc" | "__hstc" | "hsctatracking" | "mkt_tok"
            | "vero_id" | "oly_anon_id" | "oly_enc_id" | "s_cid" | "ref_src" | "ref_url" | "_ga" | "_gl" | "rb_clickid" | "spm")
}

/// The address with its tracking parameters taken out. Everything else, the order of what is
/// left and each value's percent-encoding included, stays exactly as it was; a query with
/// nothing left loses its `?`. An address that does not parse comes back as it was.
pub fn strip_tracking(url: &str) -> String {
    let Ok(mut parsed) = Url::parse(url) else {
        return url.to_owned();
    };
    let Some(query) = parsed.query() else {
        return url.to_owned();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.is_empty() && !is_tracker(pair.split('=').next().unwrap_or("")))
        .collect();
    if kept.len() == query.split('&').count() {
        return url.to_owned();
    }
    if kept.is_empty() {
        parsed.set_query(None);
    } else {
        let joined = kept.join("&");
        parsed.set_query(Some(&joined));
    }
    parsed.into()
}

/// The domains this device was told not to ask about. Registrable domains in ASCII, so the
/// same site is the same entry however it was spelled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trusted {
    domains: BTreeSet<String>,
}

/// Most domains kept, so a file that grew cannot make every click slow.
const TRUSTED_LIMIT: usize = 500;

impl Trusted {
    /// A list from stored lines, dropping any that are not a bare lowercase domain.
    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Trusted {
        let mut trusted = Trusted::default();
        for line in lines {
            trusted.add(line);
        }
        trusted
    }

    /// Whether this domain is on the list.
    pub fn contains(&self, domain: &str) -> bool {
        self.domains.contains(&domain.to_ascii_lowercase())
    }

    /// Adds a domain. False when it is not a plain domain, or the list is full.
    pub fn add(&mut self, domain: &str) -> bool {
        let domain = domain.trim().to_ascii_lowercase();
        let plain = !domain.is_empty()
            && domain.len() <= 253
            && domain.is_ascii()
            && !domain.contains(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '/' | '@' | '?' | '#' | '\\'));
        if !plain || (self.domains.len() >= TRUSTED_LIMIT && !self.domains.contains(&domain)) {
            return false;
        }
        self.domains.insert(domain);
        true
    }

    pub fn clear(&mut self) {
        self.domains.clear();
    }

    pub fn len(&self) -> usize {
        self.domains.len()
    }

    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.domains.iter().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<String> {
        find(text, &code_ranges(text)).into_iter().map(|f| f.url).collect()
    }

    fn shown(text: &str) -> Vec<&str> {
        find(text, &code_ranges(text)).into_iter().map(|f| &text[f.range]).collect()
    }

    #[test]
    fn a_table_of_tricky_text() {
        let table: &[(&str, &[&str])] = &[
            ("see https://example.com now", &["https://example.com"]),
            ("HTTP://Example.com/Path", &["HTTP://Example.com/Path"]),
            ("end of sentence https://example.com/a.", &["https://example.com/a"]),
            ("really? https://example.com/a?!", &["https://example.com/a"]),
            ("list: https://a.example/x, https://b.example/y; ok", &["https://a.example/x", "https://b.example/y"]),
            ("(https://example.com/a)", &["https://example.com/a"]),
            ("(see https://example.com/a).", &["https://example.com/a"]),
            ("https://en.wikipedia.org/wiki/Rust_(language)", &["https://en.wikipedia.org/wiki/Rust_(language)"]),
            ("(https://en.wikipedia.org/wiki/Rust_(language))", &["https://en.wikipedia.org/wiki/Rust_(language)"]),
            ("[docs](https://example.com/docs)", &["https://example.com/docs"]),
            ("[https://a.example](https://a.example)", &["https://a.example", "https://a.example"]),
            ("<https://example.com/a>", &["https://example.com/a"]),
            ("\"https://example.com/a\" and 'https://example.com/b'", &["https://example.com/a", "https://example.com/b"]),
            ("**https://example.com/bold**", &["https://example.com/bold"]),
            ("www.example.com", &["https://www.example.com"]),
            ("go to www.example.co.uk/path, please", &["https://www.example.co.uk/path"]),
            ("http://localhost:3000/x", &["http://localhost:3000/x"]),
            ("http://127.0.0.1:8080/", &["http://127.0.0.1:8080/"]),
            ("http://[::1]:8080/ok", &["http://[::1]:8080/ok"]),
            // Percent-encoding, query, fragment and ampersands are kept as typed.
            ("https://example.com/a%20b?q=1&r=%E2%9C%93#frag%20x", &["https://example.com/a%20b?q=1&r=%E2%9C%93#frag%20x"]),
            ("https://example.com/a?x=1&y=2&", &["https://example.com/a?x=1&y=2&"]),
            // Unicode paths and hosts.
            ("https://例え.jp/パス?名前=値", &["https://例え.jp/パス?名前=値"]),
            ("https://bücher.example/straße", &["https://bücher.example/straße"]),
            ("訪問https://example.com/path。次", &["https://example.com/path"]),
            ("https://example.com/a、https://example.com/b", &["https://example.com/a", "https://example.com/b"]),
            // Not links.
            ("mailto:a@example.com", &[]),
            ("file:///etc/passwd and javascript:alert(1) and intent://x#Intent;end", &[]),
            ("ftp://example.com/x", &[]),
            ("tinyclient://join#abc", &[]),
            ("a@www.example.com", &[]),
            ("awww.example.com xhttps://example.com", &[]),
            ("example.com and foo.bar/baz and file.txt", &[]),
            ("www.com www.example www. http:// https://", &[]),
            ("https://", &[]),
            ("http://.", &[]),
            ("`https://example.com/in-code`", &[]),
            ("```\nhttps://example.com/fenced\n```", &[]),
            ("a `https://example.com/x` b https://example.com/y", &["https://example.com/y"]),
            ("lone ` tick https://example.com/z", &["https://example.com/z"]),
            ("https://example.com/\u{202E}evil", &["https://example.com/"]),
            ("https://example.com/a\u{200B}b", &["https://example.com/a"]),
        ];
        for (text, want) in table {
            assert_eq!(urls(text), *want, "{text:?}");
        }
    }

    #[test]
    fn spans_are_byte_offsets_on_character_boundaries() {
        let text = "héllo 你好 😀 https://example.com/ü?x=1, and 😀 www.example.org.";
        let mut message = Message::plain("1", "Ada", text, 1, false);
        linkify(&mut message);
        let covered: Vec<(&str, &Style)> = message.spans.iter().map(|s| (&text[s.start as usize..s.end as usize], &s.style)).collect();
        assert_eq!(covered.len(), 2);
        assert_eq!(covered[0].0, "https://example.com/ü?x=1");
        assert_eq!(covered[1].0, "www.example.org");
        assert_eq!(covered[1].1, &Style::Link { url: "https://www.example.org".into() });
    }

    #[test]
    fn what_is_found_is_exactly_what_was_typed() {
        let text = "https://example.com/a%20b?q=a b";
        assert_eq!(shown(text), ["https://example.com/a%20b?q=a"]);
        let text = "https://example.com/%E3%83%91?x=%2F&y=%3D#%20";
        assert_eq!(urls(text), [text]);
    }

    #[test]
    fn linkify_is_idempotent_and_leaves_other_text_alone() {
        let mut message = Message::plain("1", "Ada", "x https://example.com y", 1, false);
        linkify(&mut message);
        linkify(&mut message);
        assert_eq!(message.spans.len(), 1);
        assert_eq!(message.text, "x https://example.com y");

        let mut system = Message::plain("2", "Ada", "https://example.com", 1, false);
        system.kind = MessageKind::System;
        linkify(&mut system);
        assert!(system.spans.is_empty());

        let mut deleted = Message::plain("3", "Ada", "https://example.com", 1, false);
        deleted.deleted = true;
        linkify(&mut deleted);
        assert!(deleted.spans.is_empty());
    }

    #[test]
    fn existing_code_spans_and_links_are_respected() {
        let text = "run https://a.example/x then https://b.example/y";
        let mut message = Message::plain("1", "Ada", text, 1, false);
        let first = text.find("https://a").unwrap() as u32;
        message.spans.push(Span { start: first, end: first + 19, style: Style::Code });
        let second = text.find("https://b").unwrap() as u32;
        message.spans.push(Span { start: second, end: text.len() as u32, style: Style::Link { url: "https://b.example/y".into() } });
        linkify(&mut message);
        assert_eq!(message.spans.len(), 2);
    }

    #[test]
    fn no_flood_of_links_and_no_endless_token() {
        let many = "https://a.example/ ".repeat(200);
        assert_eq!(urls(&many).len(), LINKS_PER_MESSAGE);
        let long = format!("https://example.com/{}", "a".repeat(LINK_LIMIT));
        assert!(urls(&long).is_empty());
        let fits = format!("https://example.com/{}", "a".repeat(LINK_LIMIT - 20));
        assert_eq!(urls(&fits).len(), 1);
    }

    #[test]
    fn the_first_link_is_the_first_one_read() {
        let mut message = Message::plain("1", "Ada", "see www.example.org and https://example.com/a", 1, false);
        linkify(&mut message);
        message.spans.reverse();
        assert_eq!(first_link(&message.text, &message.spans).as_deref(), Some("https://www.example.org"));
        assert_eq!(first_link("x", &[Span { start: 0, end: 1, style: Style::Link { url: "mailto:a@b.c".into() } }]), None);
    }

    #[test]
    fn only_the_web_is_a_web_address() {
        assert!(is_web("https://example.com/a"));
        assert!(is_web("HTTP://example.com"));
        for bad in ["mailto:a@b.c", "javascript:alert(1)", "file:///c:/x", "intent://x", "tg://resolve", "https://a b", "https://a\"b", "ftp://x", ""] {
            assert!(!is_web(bad), "{bad}");
            assert!(Destination::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_destination_names_the_real_host() {
        let d = Destination::parse("https://www.example.co.uk:8443/a%20b?q=1#x").unwrap();
        assert_eq!((d.host.as_str(), d.domain.as_str(), d.risk), ("www.example.co.uk", "example.co.uk", Risk::None));
        assert_eq!(d.url, "https://www.example.co.uk:8443/a%20b?q=1#x");

        let d = Destination::parse("https://paypal.com@evil.example/login").unwrap();
        assert_eq!((d.host.as_str(), d.domain.as_str(), d.has_userinfo), ("evil.example", "evil.example", true));

        let d = Destination::parse("https://alice.github.io/x").unwrap();
        assert_eq!(d.domain, "alice.github.io");
        assert_eq!(Destination::parse("http://127.0.0.1:80/").unwrap().domain, "127.0.0.1");
        assert_eq!(Destination::parse("http://localhost/").unwrap().domain, "localhost");
        assert_eq!(Destination::parse("https://a.b.example.org/").unwrap().domain, "example.org");
        assert_eq!(Destination::parse("https://a.b.example.com.au/").unwrap().domain, "example.com.au");
    }

    #[test]
    fn punycode_is_decoded_for_display_and_kept_for_the_wire() {
        let d = Destination::parse("https://bücher.example/straße").unwrap();
        assert_eq!(d.host, "xn--bcher-kva.example");
        assert_eq!(d.host_shown, "bücher.example");
        assert_eq!(d.domain, "xn--bcher-kva.example");
        assert_eq!(d.domain_shown, "bücher.example");
        assert_eq!(d.risk, Risk::Idn);
        assert!(d.url.starts_with("https://xn--bcher-kva.example/stra%C3%9Fe"), "{}", d.url);

        let d = Destination::parse("https://xn--bcher-kva.example/").unwrap();
        assert_eq!(d.host_shown, "bücher.example");

        // Japanese mixes Han and kana without being a lookalike.
        let d = Destination::parse("https://例え.jp/").unwrap();
        assert_eq!(d.risk, Risk::Idn);
        assert_eq!(Destination::parse("https://例え.テスト/").unwrap().risk, Risk::Idn);
    }

    #[test]
    fn homographs_are_called_out() {
        // Cyrillic a, e, o, p, c: "apple.com" and "paypal.com" in another script.
        for spoof in ["https://\u{0430}pple.com/", "https://p\u{0430}ypal.com/", "https://\u{0430}\u{0440}\u{0440}\u{04CF}\u{0435}.com/", "https://g\u{043E}\u{043E}gle.com/"] {
            let d = Destination::parse(spoof).unwrap();
            assert_eq!(d.risk, Risk::Lookalike, "{spoof}: {}", d.host_shown);
            assert!(d.host.contains("xn--"), "{}", d.host);
        }
        // Greek omicron among Latin letters.
        assert_eq!(Destination::parse("https://g\u{03BF}ogle.com/").unwrap().risk, Risk::Lookalike);
        // A whole name in Cyrillic that passes for Latin is the same trick.
        assert_eq!(Destination::parse("https://\u{0441}\u{043E}\u{0440}\u{0435}.example/").unwrap().risk, Risk::Lookalike);
        // A real Russian word is a notice, not an alarm.
        assert_eq!(Destination::parse("https://пример.example/").unwrap().risk, Risk::Idn);
        assert_eq!(Destination::parse("https://example.com/").unwrap().risk, Risk::None);
        assert_eq!(Destination::parse("https://xn--80ak6aa92e.com/").unwrap().risk, Risk::Lookalike);
    }

    #[test]
    fn tracking_parameters_go_and_nothing_else_changes() {
        let cases = [
            ("https://example.com/a?utm_source=x&id=7&fbclid=abc#top", "https://example.com/a?id=7#top"),
            ("https://example.com/a?utm_source=x&utm_medium=y", "https://example.com/a"),
            ("https://example.com/a?q=a%20b&gclid=1&r=%E2%9C%93", "https://example.com/a?q=a%20b&r=%E2%9C%93"),
            ("https://example.com/a?id=7&mc_eid=9&igshid=2&UTM_Campaign=x", "https://example.com/a?id=7"),
            ("https://example.com/a?q=1", "https://example.com/a?q=1"),
            ("https://example.com/a", "https://example.com/a"),
            ("https://example.com/a?utm_x", "https://example.com/a"),
            ("https://example.com/?s=utm_source", "https://example.com/?s=utm_source"),
        ];
        for (from, want) in cases {
            assert_eq!(strip_tracking(from), want, "{from}");
        }
        let d = Destination::parse("https://example.com/a?utm_source=x&k=v").unwrap();
        assert_eq!(d.target(true), "https://example.com/a?k=v");
        assert_eq!(d.target(false), "https://example.com/a?utm_source=x&k=v");
    }

    #[test]
    fn the_trusted_list_keeps_plain_domains_only() {
        let mut trusted = Trusted::default();
        assert!(trusted.add("Example.COM"));
        assert!(trusted.contains("example.com") && trusted.contains("EXAMPLE.com"));
        assert!(!trusted.contains("www.example.com"), "a trusted domain is the registrable one");
        for bad in ["", "  ", "a b", "a/b", "user@host", "x?y", "bücher.example"] {
            assert!(!trusted.add(bad), "{bad:?}");
        }
        assert!(trusted.add("example.com"), "again is fine");
        assert_eq!(trusted.len(), 1);
        let again = Trusted::from_lines(trusted.iter().chain(["", "junk junk"]));
        assert_eq!(again, trusted);
        trusted.clear();
        assert!(trusted.is_empty());
        let mut full = Trusted::default();
        for n in 0..TRUSTED_LIMIT + 10 {
            full.add(&format!("d{n}.example"));
        }
        assert_eq!(full.len(), TRUSTED_LIMIT);
    }
}
