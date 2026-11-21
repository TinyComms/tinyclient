//! What the shell's Enter asks of Slack's own search: one conversation, or the whole workspace.
//! One request, never a crawl: only hits in conversations the sidebar already knows come back.

use super::{seconds, thread_id, thread_of, Fault, Slack};
use crate::adapters::SearchScope;
use protocol::MessageHit;

/// Slack's search answers this many hits a page at most.
const SEARCH_HITS: &str = "100";
/// Most characters Slack's search takes in `query`.
const SEARCH_MAX_CHARS: usize = 1024;

impl Slack {
    /// The messages Slack itself finds for the pane's words, mapped like the cache's hits.
    ///
    /// `search.messages` is the documented Web API method; the same token and `d` cookie that
    /// carry every other call carry it. Unverified against a real workspace whether a web
    /// session's `xoxc` token is allowed to call it; a session Slack refuses answers in words
    /// (`reason`), never with invented hits.
    pub(super) fn search_messages(
        &self,
        query: &str,
        scope: &SearchScope,
        filters: &protocol::SearchFilters,
    ) -> Result<Vec<MessageHit>, Fault> {
        let capped: String = query.chars().take(SEARCH_MAX_CHARS).collect();
        let query = capped.trim();
        let (asked, only) = self.search_query(query, scope, filters);
        let snippet_terms = strip_modifiers(query);
        let snippet_needle = if snippet_terms.is_empty() { query } else { snippet_terms.as_str() };
        let found = self.api(
            "search.messages",
            &[
                ("query", &asked),
                ("count", SEARCH_HITS),
                ("sort", "timestamp"),
                ("sort_dir", "desc"),
                ("highlight", "false"),
            ],
        )?;
        let found = found["messages"]["matches"].as_array().map_or(&[][..], Vec::as_slice);
        let me = self.me.get().map(String::as_str).unwrap_or("");
        let mut hits = Vec::new();
        for found in found {
            let (Some(channel), Some(ts)) = (found["channel_id"].as_str(), found["ts"].as_str()) else {
                continue;
            };
            // Slack's own filter is the words; this keeps to them if it widened the page.
            if only.as_deref().is_some_and(|only| only != channel) {
                continue;
            }
            // A hit in a conversation the sidebar does not know cannot be opened. A public
            // channel the user never joined is not recorded by a search.
            let Ok(Some(known)) = self.ctx.known(channel) else {
                continue;
            };
            let root = found["thread_ts"].as_str().filter(|root| *root != ts);
            let place = root.map_or_else(|| channel.to_owned(), |root| thread_id(channel, root));
            let title = match root {
                // The thread has a row of its own to land on; recording it asks Slack nothing.
                Some(root) if self.thread_known(channel, root).is_ok() => {
                    self.ctx.known(&place).ok().flatten().map_or_else(|| known.title.clone(), |thread| thread.title)
                }
                _ => known.title.clone(),
            };
            let sender = found["user"].as_str().map_or_else(
                || found["username"].as_str().unwrap_or("").to_owned(),
                |id| self.cached(id).map_or_else(|| found["username"].as_str().unwrap_or(id).to_owned(), |user| user.name),
            );
            // The hit's own markup as the words the cache would hold.
            let mut rich = super::text::Rich::new(self);
            rich.mrkdwn(found["text"].as_str().unwrap_or(""));
            rich.finish();
            hits.push(MessageHit {
                conversation: self.ctx.key(&place),
                message_id: ts.to_owned(),
                sent: seconds(ts),
                sender,
                snippet: crate::store::snippet(&rich.text, snippet_needle),
                title,
                space_title: known.space_title.clone(),
                outgoing: !me.is_empty() && found["user"].as_str() == Some(me),
            });
        }
        Ok(hits)
    }

    /// Slack's own words for a scope and structured filters: `in:#channel`, `from:<@U…>`,
    /// `has:link`, `before:YYYY-MM-DD`, `after:YYYY-MM-DD`. Never appends a duplicate modifier
    /// if the user already typed one in `query`.
    fn search_query(&self, query: &str, scope: &SearchScope, filters: &protocol::SearchFilters) -> (String, Option<String>) {
        let (scope_in, only) = match scope {
            SearchScope::Conversation(id) => {
                let channel = thread_of(id).map_or(id.as_str(), |(channel, _)| channel);
                let filter = self.ctx.known(channel).ok().flatten().and_then(|known| {
                    if known.title.starts_with('#') {
                        return Some(known.title);
                    }
                    // Slack names a direct conversation's partner `@handle` in a search.
                    self.state()
                        .partners
                        .iter()
                        .find(|(_, place)| place.as_str() == channel)
                        .and_then(|(person, _)| {
                            self.cached(person).map(|user| format!("@{}", user.handle)).filter(|handle| handle.len() > 1)
                        })
                });
                let only = if has_modifier(query, "in:") { None } else { Some(channel.to_owned()) };
                (filter, only)
            }
            _ => (None, None),
        };
        let me = self.me.get().map(String::as_str);
        let asked = format_slack_query(query, filters, scope_in.as_deref(), me);
        (asked, only)
    }
}

/// Known Slack search modifier prefixes.
const MODIFIERS: &[&str] = &[
    "in:", "from:", "to:", "has:", "is:", "before:", "after:", "on:", "during:", "with:",
];

/// True when `query` already contains a whitespace-separated token with `prefix` (case-insensitive).
pub(super) fn has_modifier(query: &str, prefix: &str) -> bool {
    let want = prefix.to_ascii_lowercase();
    query
        .split_whitespace()
        .any(|token| token.trim_start_matches('-').to_ascii_lowercase().starts_with(&want))
}

/// Returns the plain search words with Slack modifier tokens (`in:`, `from:`, `has:`, etc.) removed.
pub(super) fn strip_modifiers(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|token| {
            let lower = token.trim_start_matches('-').to_ascii_lowercase();
            !MODIFIERS.iter().any(|prefix| lower.starts_with(prefix))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Formats a unix timestamp (seconds) as `YYYY-MM-DD` UTC for Slack's `before:` / `after:` modifiers.
fn format_ymd(unix_secs: i64) -> String {
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = unix_secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Builds the full Slack `search.messages` query string from user text, structured filters, and scope.
pub(super) fn format_slack_query(
    query: &str,
    filters: &protocol::SearchFilters,
    scope_in: Option<&str>,
    me_id: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    let trimmed = query.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_owned());
    }
    if let Some(in_target) = scope_in.filter(|s| !s.is_empty()) {
        if !has_modifier(trimmed, "in:") {
            parts.push(format!("in:{in_target}"));
        }
    }
    if let Some(from) = filters.from.as_ref().filter(|k| !k.id.is_empty()) {
        if !has_modifier(trimmed, "from:") {
            parts.push(format!("from:<@{}>", from.id));
        }
    }
    if let Some(has) = filters.has {
        if !has_modifier(trimmed, "has:") {
            let word = match has {
                protocol::SearchHas::Links => "link",
                protocol::SearchHas::Files => "file",
                protocol::SearchHas::Images => "image",
            };
            parts.push(format!("has:{word}"));
        }
    }
    if let Some(before) = filters.before.filter(|ts| *ts > 0) {
        if !has_modifier(trimmed, "before:") {
            parts.push(format!("before:{}", format_ymd(before)));
        }
    }
    if let Some(after) = filters.after.filter(|ts| *ts > 0) {
        if !has_modifier(trimmed, "after:") {
            parts.push(format!("after:{}", format_ymd(after)));
        }
    }
    if filters.mentions_me {
        if let Some(me) = me_id.filter(|s| !s.is_empty()) {
            let mention = format!("<@{me}>");
            if !trimmed.contains(&mention) {
                parts.push(mention);
            }
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Key, Network, SearchFilters, SearchHas};

    #[test]
    fn slack_search_query_formats_filters_and_avoids_duplicate_modifiers() {
        let filters = SearchFilters {
            from: Some(Key::new(&AccountKey { network: Network::Slack, account: "T1".into() }, "U0ANA")),
            has: Some(SearchHas::Links),
            before: Some(1_767_225_600), // 2026-01-01
            after: Some(1_735_689_600),  // 2025-01-01
            mentions_me: true,
        };
        let formatted = format_slack_query("deploy", &filters, Some("#general"), Some("U0ME"));
        assert_eq!(
            formatted,
            "deploy in:#general from:<@U0ANA> has:link before:2026-01-01 after:2025-01-01 <@U0ME>"
        );

        // User already typed `in:#ops` and `has:pin` -> do not append duplicates.
        let with_inline = format_slack_query("in:#ops has:pin during:2026-11 deploy", &filters, Some("#general"), None);
        assert!(with_inline.contains("in:#ops"));
        assert!(!with_inline.contains("in:#general"));
        assert!(with_inline.contains("has:pin"));
        assert!(!with_inline.contains("has:link"));
        assert!(with_inline.contains("from:<@U0ANA>"));

        assert_eq!(strip_modifiers("from:@ana has:link during:2026-11 deploy"), "deploy");
        assert_eq!(strip_modifiers("-in:#random is:thread status check"), "status check");
    }
}
