//! Text fallback for URL-only Wikipedia embeds. No arbitrary website is fetched.

use super::Pane;
use gpui::Context;
use protocol::Preview;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const HELD: usize = 128;
const ACTIVE: usize = 2;
const RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
struct Summary {
    title: String,
    description: String,
}

#[derive(Default)]
pub(super) struct Cache {
    ready: HashMap<String, (Option<Summary>, Instant)>,
    pending: HashSet<String>,
}

/// Recognize ordinary HTTPS article links, including mobile and translated pages.
fn article(link: &str) -> Option<(String, String)> {
    let url = url::Url::parse(link).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return None;
    }
    let subdomain = url.host_str()?.strip_suffix(".wikipedia.org")?;
    let language = subdomain.strip_suffix(".m").unwrap_or(subdomain);
    if language.is_empty() || !language.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') {
        return None;
    }
    let raw = url.path().strip_prefix("/wiki/")?;
    let mut bytes = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        if raw.as_bytes()[at] == b'%' {
            bytes.push(u8::from_str_radix(raw.get(at + 1..at + 3)?, 16).ok()?);
            at += 3;
        } else {
            bytes.push(raw.as_bytes()[at]);
            at += 1;
        }
    }
    let title = String::from_utf8(bytes).ok()?;
    if title.is_empty() || title.len() > 512 || title.chars().any(char::is_control) || title.contains(':') {
        return None;
    }
    let mut encoded = String::new();
    for byte in title.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    Some((format!("https://{language}.wikipedia.org/api/rest_v1/page/summary/{encoded}"), title.replace('_', " ")))
}

fn summary(json: &str) -> Option<Summary> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if !matches!(value.get("type")?.as_str()?, "standard" | "disambiguation") {
        return None;
    }
    let title: String = value.get("title")?.as_str()?.chars().take(256).collect();
    let description: String = value.get("extract")?.as_str()?.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(600).collect();
    if title.trim().is_empty() || description.is_empty() {
        return None;
    }
    Some(Summary { title, description })
}

fn fetch(endpoint: &str) -> Option<Summary> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).build())
        .timeout_global(Some(Duration::from_secs(8)))
        .max_redirects(0)
        .build().into();
    let mut answer = agent.get(endpoint)
        .header("User-Agent", "TinyClient/0.2.5 (https://github.com/itz4blitz/tinyclient)")
        .header("Accept", "application/json")
        .call().ok()?;
    if answer.status() != 200 {
        return None;
    }
    let text = answer.body_mut().with_config().limit(256 * 1024).read_to_string().ok()?;
    summary(&text)
}

impl Pane {
    pub(super) fn wikipedia_preview(&mut self, original: &Preview, cx: &mut Context<Self>) -> Preview {
        let mut preview = original.clone();
        if !preview.title.trim().is_empty() || !preview.description.trim().is_empty() || preview.image.is_some() {
            return preview;
        }
        let Some((endpoint, title)) = article(&preview.url) else {
            return preview;
        };
        preview.site = "Wikipedia".into();
        preview.title = title;
        if let Some((Some(summary), _)) = self.wiki.ready.get(&endpoint) {
            preview.title = summary.title.clone();
            preview.description = summary.description.clone();
            return preview;
        }
        // The title is useful even offline or when automatic preview loading is disabled.
        let retry = self.wiki.ready.get(&endpoint).is_none_or(|(_, when)| when.elapsed() >= RETRY);
        if !crate::gui::app::prefs::choices().images || !retry || self.wiki.pending.contains(&endpoint) || self.wiki.pending.len() >= ACTIVE {
            return preview;
        }
        self.wiki.pending.insert(endpoint.clone());
        cx.spawn(async move |this, cx| {
            let requested = endpoint.clone();
            let answer = cx.background_executor().spawn(async move { fetch(&requested) }).await;
            let _ = this.update(cx, |this, cx| {
                this.wiki.pending.remove(&endpoint);
                if this.wiki.ready.len() >= HELD {
                    if let Some(oldest) = this.wiki.ready.iter().min_by_key(|(_, (_, when))| when).map(|(key, _)| key.clone()) {
                        this.wiki.ready.remove(&oldest);
                    }
                }
                let top = this.list.logical_scroll_top();
                let pinned = this.pinned();
                let changed = answer.is_some();
                this.wiki.ready.insert(endpoint.clone(), (answer, Instant::now()));
                if changed {
                    // Summary text changes a row's height; remeasure without losing its anchor.
                    for (ix, item) in this.items.iter().enumerate() {
                        if item.message().is_some_and(|message| message.previews.iter().any(|p| article(&p.url).is_some_and(|(key, _)| key == endpoint))) {
                            this.list.splice(ix..ix + 1, 1);
                        }
                    }
                    if !pinned {
                        this.list.scroll_to(top);
                    } else if this.is_thread() {
                        this.list.scroll_to_end();
                    }
                }
                cx.notify();
            });
        }).detach();
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_urls_keep_language_and_encode_titles_without_query_or_fragment() {
        assert_eq!(article("https://en.wikipedia.org/wiki/Miller_columns#History"), Some(("https://en.wikipedia.org/api/rest_v1/page/summary/Miller_columns".into(), "Miller columns".into())));
        assert_eq!(article("https://fr.m.wikipedia.org/wiki/%C3%89cran_tactile?x=1"), Some(("https://fr.wikipedia.org/api/rest_v1/page/summary/%C3%89cran_tactile".into(), "Écran tactile".into())));
        assert_eq!(article("https://en.wikipedia.org/wiki/AC/DC").unwrap().0, "https://en.wikipedia.org/api/rest_v1/page/summary/AC%2FDC");
    }

    #[test]
    fn unrelated_hosts_credentials_special_pages_and_invalid_titles_are_not_fetched() {
        for link in ["https://wikipedia.org.evil.test/wiki/Test", "https://evil.wikipedia.org.evil.test/wiki/Test", "http://en.wikipedia.org/wiki/Test", "https://user@en.wikipedia.org/wiki/Test", "https://en.wikipedia.org:444/wiki/Test", "https://en.wikipedia.org/w/index.php?title=Test", "https://en.wikipedia.org/wiki/Special:Random", "https://en.wikipedia.org/wiki/%00", "https://en.wikipedia.org/wiki/%XX", "https://en.wikipedia.org/wiki/"] {
            assert!(article(link).is_none(), "{link}");
        }
    }

    #[test]
    fn extracts_are_plain_bounded_text_and_api_errors_are_not_summaries() {
        assert_eq!(summary(r#"{"type":"standard","title":"Miller columns","extract":"A browsing\n technique."}"#), Some(Summary { title: "Miller columns".into(), description: "A browsing technique.".into() }));
        assert!(summary(r#"{"type":"https://mediawiki.org/error","title":"Not found"}"#).is_none());
        assert!(summary(r#"{"type":"standard","title":"Empty","extract":""}"#).is_none());
        let long = serde_json::json!({"type":"standard","title":"é".repeat(300),"extract":"é".repeat(1000)});
        let got = summary(&long.to_string()).unwrap();
        assert_eq!((got.title.chars().count(), got.description.chars().count()), (256, 600));
    }
}
