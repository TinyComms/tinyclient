//! What the shell's Enter asks of Slack's own search: one conversation, or the whole workspace.

use super::{seconds, thread_id, thread_of, Fault, Slack};
use crate::adapters::{AccountFilesPage, SearchPage, SearchScope};
use protocol::{MessageHit, SearchSort, SharedItem};
use serde_json::Value;

/// Slack's search answers this many hits a page at most.
const SEARCH_HITS: &str = "100";
/// Most characters Slack's search takes in `query`.
const SEARCH_MAX_CHARS: usize = 1024;

impl Slack {
    /// The messages Slack itself finds for the pane's words, mapped like the cache's hits.
    ///
    /// Supports paging (`page=N`), sort order (`timestamp` vs `score`), unjoined public channel
    /// hits (only when Slack sets `is_channel` and `is_private` false), and falls back to the web
    /// client's `search.modules` (`module=messages`) if `search.messages` refuses with
    /// `not_allowed_token_type`.
    pub(super) fn search_messages(
        &self,
        query: &str,
        scope: &SearchScope,
        filters: &protocol::SearchFilters,
        cursor: Option<&str>,
    ) -> Result<SearchPage, Fault> {
        let capped: String = query.chars().take(SEARCH_MAX_CHARS).collect();
        let query = capped.trim();
        let (asked, only) = self.search_query(query, scope, filters);
        let snippet_terms = strip_modifiers(query);
        let snippet_needle = if snippet_terms.is_empty() { query } else { snippet_terms.as_str() };
        let page_num: u32 = cursor.and_then(|c| c.parse().ok()).unwrap_or(1).max(1);
        let page_str = page_num.to_string();
        let sort_param = match filters.sort {
            SearchSort::Newest | SearchSort::Oldest => "timestamp",
            SearchSort::Relevance => "score",
        };
        let found = match self.api(
            "search.messages",
            &[
                ("query", &asked),
                ("count", SEARCH_HITS),
                ("page", &page_str),
                ("sort", sort_param),
                ("sort_dir", "desc"),
                ("highlight", "false"),
            ],
        ) {
            Ok(resp) => resp,
            Err(Fault::Slack(ref err)) if err == "not_allowed_token_type" => {
                // Unverified web-client fallback when `search.messages` refuses session tokens.
                // If `search.modules` also fails, preserve the original refusal wording.
                match self.api(
                    "search.modules",
                    &[
                        ("module", "messages"),
                        ("query", &asked),
                        ("count", SEARCH_HITS),
                        ("page", &page_str),
                        ("sort", sort_param),
                        ("sort_dir", "desc"),
                    ],
                ) {
                    Ok(resp) => resp,
                    Err(_) => return Err(Fault::Slack("not_allowed_token_type".to_owned())),
                }
            }
            Err(fault) => return Err(fault),
        };
        let raw_matches: Vec<Value> = if let Some(arr) = found["messages"]["matches"].as_array() {
            arr.clone()
        } else if let Some(items) = found["items"].as_array() {
            items
                .iter()
                .flat_map(|item| {
                    if let Some(msgs) = item["messages"].as_array() {
                        msgs.clone()
                    } else {
                        vec![item.clone()]
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let paging = if found["messages"]["paging"].is_object() {
            &found["messages"]["paging"]
        } else {
            &found["pagination"]
        };
        let page = paging["page"].as_u64().unwrap_or(u64::from(page_num)) as u32;
        let pages = paging["pages"]
            .as_u64()
            .or_else(|| paging["page_count"].as_u64())
            .unwrap_or(1) as u32;
        let next_cursor = (page < pages).then(|| (page + 1).to_string());
        let space_name = self
            .ctx
            .hub
            .store()
            .spaces(Some(protocol::Network::Slack))
            .ok()
            .and_then(|spaces| spaces.into_iter().find(|s| s.key.id == self.ctx.account.account))
            .map(|s| s.name)
            .unwrap_or_default();
        let me = self.me.get().map(String::as_str).unwrap_or("");
        let mut hits = Vec::new();
        for found in &raw_matches {
            let channel = found["channel_id"]
                .as_str()
                .or_else(|| found["channel"]["id"].as_str())
                .filter(|c| !c.is_empty());
            let (Some(channel), Some(ts)) = (channel, found["ts"].as_str()) else {
                continue;
            };
            // Slack's own filter is the words; this keeps to them if it widened the page.
            if only.as_deref().is_some_and(|only| only != channel) {
                continue;
            }
            let (known_title, space_title) = match self.ctx.known(channel) {
                Ok(Some(known)) => (known.title, known.space_title),
                _ => {
                    // Keep an unjoined hit only when Slack says it is a public channel.
                    // A missing `is_private` is not public. Clicking opens a read-only preview.
                    let ch = &found["channel"];
                    let public_name = ch["name"]
                        .as_str()
                        .filter(|n| !n.is_empty())
                        .filter(|_| {
                            ch["is_channel"] == true
                                && ch["is_private"] == false
                                && ch["is_im"] != true
                                && ch["is_mpim"] != true
                                && ch["is_group"] != true
                        });
                    let Some(name) = public_name else {
                        continue;
                    };
                    (format!("#{}", name.trim_start_matches('#')), space_name.clone())
                }
            };
            let root = found["thread_ts"].as_str().filter(|root| *root != ts);
            let place = root.map_or_else(|| channel.to_owned(), |root| thread_id(channel, root));
            let title = match root {
                // The thread has a row of its own to land on; recording it asks Slack nothing.
                Some(root) if self.ctx.known(channel).ok().flatten().is_some() && self.thread_known(channel, root).is_ok() => {
                    self.ctx.known(&place).ok().flatten().map_or_else(|| known_title.clone(), |thread| thread.title)
                }
                _ => known_title,
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
                space_title,
                outgoing: !me.is_empty() && found["user"].as_str() == Some(me),
                age_restricted: false,
            });
        }
        Ok(SearchPage { hits, cursor: next_cursor, total: None })
    }

    /// Searches workspace files (`search.files`, with fallback to `search.modules` `module=files`).
    pub(super) fn search_files(
        &self,
        query: &str,
        scope: &SearchScope,
        sort: SearchSort,
        cursor: Option<&str>,
    ) -> Result<AccountFilesPage, Fault> {
        let capped: String = query.chars().take(SEARCH_MAX_CHARS).collect();
        let query = capped.trim();
        let (asked, _) = self.search_query(query, scope, &protocol::SearchFilters::default());
        let page_num: u32 = cursor.and_then(|c| c.parse().ok()).unwrap_or(1).max(1);
        let page_str = page_num.to_string();
        let sort_param = match sort {
            SearchSort::Newest | SearchSort::Oldest => "timestamp",
            SearchSort::Relevance => "score",
        };
        let found = match self.api(
            "search.files",
            &[
                ("query", &asked),
                ("count", "50"),
                ("page", &page_str),
                ("sort", sort_param),
                ("sort_dir", "desc"),
            ],
        ) {
            Ok(resp) => resp,
            Err(Fault::Slack(ref err)) if err == "not_allowed_token_type" => {
                match self.api(
                    "search.modules",
                    &[
                        ("module", "files"),
                        ("query", &asked),
                        ("count", "50"),
                        ("page", &page_str),
                        ("sort", sort_param),
                        ("sort_dir", "desc"),
                    ],
                ) {
                    Ok(resp) => resp,
                    Err(_) => return Err(Fault::Slack("not_allowed_token_type".to_owned())),
                }
            }
            Err(fault) => return Err(fault),
        };
        let files = found["files"]["matches"]
            .as_array()
            .or_else(|| found["items"].as_array())
            .map_or(&[][..], Vec::as_slice);
        let mut items = Vec::with_capacity(files.len());
        let mut media = Vec::new();
        let mut variants = Vec::new();
        for file in files {
            let Some(id) = file["id"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            let title = file["title"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| file["name"].as_str().filter(|s| !s.is_empty()))
                .unwrap_or("untitled")
                .to_owned();
            let sent = file["created"]
                .as_i64()
                .or_else(|| file["timestamp"].as_i64())
                .unwrap_or(0);
            let sender = file["user"]
                .as_str()
                .filter(|u| !u.is_empty())
                .map(|u| self.user(u).name)
                .or_else(|| file["username"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let permalink = file["permalink"]
                .as_str()
                .map(str::trim)
                .filter(|u| u.starts_with("https://"))
                .unwrap_or("")
                .to_owned();
            if self.is_external(file) {
                let url = file["url_private"]
                    .as_str()
                    .map(str::trim)
                    .filter(|u| u.starts_with("https://"))
                    .unwrap_or(&permalink)
                    .to_owned();
                items.push(SharedItem {
                    message_id: id.to_owned(),
                    sent,
                    sender,
                    attachment: None,
                    url,
                    title,
                });
                continue;
            }
            let attachment = self.file(file, &mut media, &mut variants);
            items.push(SharedItem {
                message_id: id.to_owned(),
                sent,
                sender,
                attachment,
                url: permalink,
                title,
            });
        }
        let mut sized = variants.iter().filter(|(_, sizes)| !sizes.is_empty());
        if let Err(err) = self
            .ctx
            .media(&media)
            .and_then(|()| sized.try_for_each(|(id, sizes)| self.ctx.media_variants(id, sizes)))
        {
            tracing::warn!("chatd: Slack: {err}");
        }
        let paging = if found["files"]["paging"].is_object() {
            &found["files"]["paging"]
        } else {
            &found["pagination"]
        };
        let page = paging["page"].as_u64().unwrap_or(u64::from(page_num)) as u32;
        let pages = paging["pages"]
            .as_u64()
            .or_else(|| paging["page_count"].as_u64())
            .unwrap_or(1) as u32;
        let more = page < pages;
        let next_cursor = more.then(|| (page + 1).to_string());
        Ok(AccountFilesPage { items, cursor: next_cursor, more })
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

/// True when `query` already contains an unquoted whitespace-separated token with `prefix` (case-insensitive).
pub(super) fn has_modifier(query: &str, prefix: &str) -> bool {
    let want = prefix.to_ascii_lowercase();
    let mut in_quotes = false;
    for token in query.split_whitespace() {
        let starts_quoted = in_quotes || token.starts_with('"');
        let quotes = token.bytes().filter(|&b| b == b'"').count();
        if quotes % 2 == 1 {
            in_quotes = !in_quotes;
        }
        if !starts_quoted && token.trim_start_matches('-').to_ascii_lowercase().starts_with(&want) {
            return true;
        }
    }
    false
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
                protocol::SearchHas::Links | protocol::SearchHas::Embeds => "link",
                protocol::SearchHas::Files => "file",
                protocol::SearchHas::Images | protocol::SearchHas::Stickers => "image",
                protocol::SearchHas::Videos => "video",
                protocol::SearchHas::Sounds => "audio",
                protocol::SearchHas::Polls => "poll",
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
            sort: SearchSort::Newest,
            ..SearchFilters::default()
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
