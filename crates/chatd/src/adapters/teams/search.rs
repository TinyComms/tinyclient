//! Server-side search for Microsoft Teams via Microsoft Search / Substrate (`regionGtms.search`),
//! supporting account-wide search, team-scoped search, in-conversation search, structured filters
//! (`from`, `has`, `before`, `after`, `mentions_me`), and jumping into channel reply chains.

use super::{chain_id, id_of, millis, seconds, thread_of, Audience, Auth, Body, Fault, Teams, SUBSTRATE};
use crate::adapters::SearchScope;
use protocol::{MessageHit, SearchFilters, SearchHas};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Drop search commands queued longer than this so typing bursts do not pile up.
const SEARCH_PATIENCE: Duration = Duration::from_secs(10);
/// Minimum interval between Teams search requests (rate-limited separately from chat).
const SEARCH_GAP: Duration = Duration::from_millis(500);
/// Maximum query length accepted by Teams search.
const SEARCH_MAX_CHARS: usize = 1024;
/// Maximum hits requested per page.
const SEARCH_PAGE_SIZE: usize = 25;

/// Strips control characters, trims, and caps free-text search input to [`SEARCH_MAX_CHARS`].
fn sanitize_search_text(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(SEARCH_MAX_CHARS)
        .collect()
}

/// Sanitizes a structured KQL value so quotes, backslashes, or control chars cannot break out of
/// a quoted filter token.
fn sanitize_kql_value(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Strips Substrate highlight tags (`<c0>`, `</c0>`, `<ddd/>`) and Teams HTML into plain text.
pub(super) fn clean_snippet(raw: &str) -> String {
    let stripped = raw
        .replace("<c0>", "")
        .replace("</c0>", "")
        .replace("<ddd/>", "…")
        .replace("<ddd />", "…");
    let text = super::html::parse(&stripped, &super::people::Nobody, "").text;
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Builds the Substrate / Microsoft Search KQL query string from user words and structured filters.
pub(super) fn build_query_string(
    query: &str,
    from_term: Option<&str>,
    filters: &SearchFilters,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    let trimmed = sanitize_search_text(query);
    if !trimmed.is_empty() {
        parts.push(trimmed);
    }
    if let Some(from) = from_term.map(sanitize_kql_value).filter(|s| !s.is_empty()) {
        parts.push(format!("from:\"{from}\""));
    }
    match filters.has {
        Some(SearchHas::Files | SearchHas::Images) => parts.push("hasAttachment:true".to_owned()),
        Some(SearchHas::Links) => parts.push("hasLinks:true".to_owned()),
        None => {}
    }
    if filters.mentions_me {
        parts.push("mentionsMe:true".to_owned());
    }
    if let Some(after) = filters.after.filter(|&t| t > 0) {
        parts.push(format!("sent>={}", super::iso_utc(after)));
    }
    if let Some(before) = filters.before.filter(|&t| t > 0) {
        parts.push(format!("sent<={}", super::iso_utc(before)));
    }
    parts.join(" ")
}

/// Extracts raw hit objects from either Microsoft Search Substrate (`EntitySets[].ResultSets[].Results[]`)
/// or Teams' `msgsearch` (`Groups[].Results[]`, `Results[]`, `results[]`) response envelopes.
pub(super) fn extract_raw_hits(body: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    if let Some(entity_sets) = body["EntitySets"].as_array() {
        for es in entity_sets {
            for rs in es["ResultSets"].as_array().into_iter().flatten() {
                for r in rs["Results"].as_array().into_iter().flatten() {
                    out.push(r);
                }
            }
        }
    }
    if out.is_empty() {
        if let Some(groups) = body["Groups"].as_array() {
            for g in groups {
                for r in g["Results"].as_array().into_iter().flatten() {
                    out.push(r);
                }
            }
        }
    }
    if out.is_empty() {
        for key in ["Results", "results", "hits", "value"] {
            if let Some(arr) = body[key].as_array() {
                out.extend(arr.iter());
                break;
            }
        }
    }
    out
}

/// Parsed fields of a single Teams search hit before resolving local conversation metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedHit {
    pub thread_id: String,
    pub message_id: String,
    pub parent_id: Option<String>,
    pub sender_id: String,
    pub sender_name: String,
    pub sent: i64,
    pub text: String,
    pub title: String,
    pub space_title: String,
    pub team_id: String,
    pub has_files: Option<bool>,
    pub has_images: Option<bool>,
    pub has_links: Option<bool>,
    pub mentions_me: Option<bool>,
}

pub(super) fn parse_hit(raw: &Value, me: &str) -> Option<ParsedHit> {
    let src = if raw["Source"].is_object() {
        &raw["Source"]
    } else if raw["resource"].is_object() {
        &raw["resource"]
    } else {
        raw
    };
    let str_of = |candidates: &[&Value]| -> String {
        candidates
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_owned()
    };
    let id_field = |candidates: &[&Value]| -> String {
        candidates
            .iter()
            .map(|v| id_of(v))
            .find(|s| !s.is_empty())
            .unwrap_or_default()
    };

    let raw_thread = str_of(&[
        &src["ThreadId"],
        &src["threadId"],
        &src["ConversationId"],
        &src["conversationId"],
        &raw["ThreadId"],
        &raw["threadId"],
        &raw["ConversationId"],
        &raw["conversationId"],
    ]);
    if raw_thread.is_empty() {
        return None;
    }
    let (thread_id, link_root) = match super::chain_of(&raw_thread) {
        Some((ch, root)) => (ch.to_owned(), Some(root.to_owned())),
        None => (raw_thread, None),
    };
    let message_id = id_field(&[
        &src["MessageId"],
        &src["messageId"],
        &src["ClientMessageId"],
        &src["Id"],
        &src["id"],
        &raw["MessageId"],
        &raw["messageId"],
        &raw["Id"],
        &raw["id"],
    ]);
    if message_id.is_empty() {
        return None;
    }
    let parent_raw = id_field(&[
        &src["ParentMessageId"],
        &src["parentMessageId"],
        &src["RootMessageId"],
        &src["rootMessageId"],
        &raw["ParentMessageId"],
        &raw["parentMessageId"],
    ]);
    let parent_id = link_root
        .or_else(|| (!parent_raw.is_empty()).then_some(parent_raw))
        .filter(|p| p != &message_id && millis(p) > 0);

    let sender_id = str_of(&[
        &src["From"]["Mri"],
        &src["From"]["mri"],
        &src["SenderId"],
        &src["senderId"],
        &src["FromMri"],
        &src["from"],
        &raw["From"]["Mri"],
        &raw["SenderId"],
    ]);
    let sender_id = sender_id.rsplit('/').next().unwrap_or(&sender_id).to_owned();
    let sender_name = str_of(&[
        &src["From"]["Name"],
        &src["From"]["DisplayName"],
        &src["From"]["displayName"],
        &src["SenderDisplayName"],
        &src["imDisplayName"],
        &src["imdisplayname"],
        &raw["From"]["Name"],
        &raw["SenderDisplayName"],
    ]);

    let sent_iso = str_of(&[
        &src["DateTimeSent"],
        &src["ComposeTime"],
        &src["composeTime"],
        &src["OriginalArrivalTime"],
        &raw["DateTimeSent"],
    ]);
    let sent = match millis(&message_id) / 1000 {
        0 => seconds(&sent_iso),
        s => s,
    };

    let raw_text = str_of(&[
        &raw["HitHighlightedSummary"],
        &raw["Summary"],
        &src["HitHighlightedSummary"],
        &src["Summary"],
        &src["Content"],
        &src["content"],
        &src["Body"]["Content"],
        &raw["Content"],
    ]);
    let text = clean_snippet(&raw_text);
    let title = str_of(&[
        &src["ThreadTopic"],
        &src["ChannelName"],
        &src["ConversationTitle"],
        &src["Topic"],
        &raw["ThreadTopic"],
        &raw["ChannelName"],
    ]);
    let space_title = str_of(&[&src["TeamName"], &src["teamName"], &raw["TeamName"]]);
    let team_id = str_of(&[&src["TeamId"], &src["teamId"], &src["GroupId"], &raw["TeamId"]]);

    let has_files = if src["HasAttachments"] == true
        || raw["HasAttachments"] == true
        || src["Attachments"].as_array().is_some_and(|a| !a.is_empty())
    {
        Some(true)
    } else if src["HasAttachments"] == false
        || raw["HasAttachments"] == false
        || src["Attachments"].as_array().is_some_and(|a| a.is_empty())
    {
        Some(false)
    } else {
        None
    };
    let has_images = if src["HasImages"] == true
        || raw["HasImages"] == true
        || raw_text.contains("<img")
        || src["Attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|a| a["ContentType"].as_str().is_some_and(|c| c.starts_with("image/")))
    {
        Some(true)
    } else if src["HasImages"] == false || raw["HasImages"] == false {
        Some(false)
    } else {
        None
    };
    let has_links = if src["HasLinks"] == true
        || raw["HasLinks"] == true
        || text.contains("https://")
        || text.contains("http://")
        || raw_text.contains("<a ")
    {
        Some(true)
    } else if src["HasLinks"] == false || raw["HasLinks"] == false {
        Some(false)
    } else {
        None
    };
    let mentions_me = if src["MentionsMe"] == true
        || raw["MentionsMe"] == true
        || (!me.is_empty()
            && src["Mentions"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|m| m["Mri"].as_str() == Some(me) || m.as_str() == Some(me)))
    {
        Some(true)
    } else if src["MentionsMe"] == false
        || raw["MentionsMe"] == false
        || (!me.is_empty() && src["Mentions"].as_array().is_some())
    {
        Some(false)
    } else {
        None
    };

    Some(ParsedHit {
        thread_id,
        message_id,
        parent_id,
        sender_id,
        sender_name,
        sent,
        text,
        title,
        space_title,
        team_id,
        has_files,
        has_images,
        has_links,
        mentions_me,
    })
}

fn matches_filters(hit: &ParsedHit, filters: &SearchFilters) -> bool {
    if let Some(ref from) = filters.from {
        if !hit.sender_id.is_empty() && hit.sender_id != from.id {
            return false;
        }
    }
    match filters.has {
        Some(SearchHas::Files) if hit.has_files == Some(false) => return false,
        Some(SearchHas::Images) if hit.has_images == Some(false) && hit.has_files != Some(true) => return false,
        Some(SearchHas::Links) if hit.has_links == Some(false) => return false,
        _ => {}
    }
    if filters.mentions_me && hit.mentions_me == Some(false) {
        return false;
    }
    if let Some(before) = filters.before.filter(|&t| t > 0) {
        if hit.sent > 0 && hit.sent > before {
            return false;
        }
    }
    if let Some(after) = filters.after.filter(|&t| t > 0) {
        if hit.sent > 0 && hit.sent < after {
            return false;
        }
    }
    true
}

impl Teams {
    /// Server-side Teams message search (Microsoft Search / Substrate) across a conversation,
    /// a team (Space), or the entire account, with structured filters and reply-chain mapping.
    pub(super) fn search_messages(
        &self,
        query: &str,
        scope: &SearchScope,
        filters: &SearchFilters,
        cursor: Option<&str>,
        at: Instant,
    ) -> Result<Vec<MessageHit>, Fault> {
        if at.elapsed() > SEARCH_PATIENCE {
            return Ok(Vec::new());
        }
        // Pace search independently from chat calls.
        {
            let mut state = self.state();
            if let Some(last) = state.search_at {
                let elapsed = last.elapsed();
                if elapsed < SEARCH_GAP {
                    drop(state);
                    std::thread::sleep(SEARCH_GAP - elapsed);
                    self.state().search_at = Some(Instant::now());
                } else {
                    state.search_at = Some(Instant::now());
                }
            } else {
                state.search_at = Some(Instant::now());
            }
        }

        let from_term = filters.from.as_ref().map(|k| {
            self.cached(&k.id)
                .map(|known| if !known.mail.is_empty() { known.mail } else { known.name })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| k.id.clone())
        });
        let query_string = build_query_string(query, from_term.as_deref(), filters);
        if query_string.is_empty() {
            return Ok(Vec::new());
        }

        let offset: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
        let mut scope_filter = serde_json::Map::new();
        match scope {
            SearchScope::Conversation(id) => {
                scope_filter.insert("ThreadId".to_owned(), Value::String(thread_of(id).to_owned()));
            }
            SearchScope::Space(space_id) => {
                scope_filter.insert("TeamId".to_owned(), Value::String(space_id.clone()));
                if let Some(group) = self.ctx.note(&format!("group:{space_id}")) {
                    if !group.is_empty() {
                        scope_filter.insert("GroupId".to_owned(), Value::String(group));
                    }
                }
            }
            SearchScope::Account => {}
        }

        let display_query = sanitize_search_text(query);
        let mut request_obj = json!({
            "EntityRequests": [{
                "EntityType": "Message",
                "ContentSources": ["Teams"],
                "Query": {
                    "QueryString": query_string,
                    "DisplayQueryString": display_query,
                },
                "From": offset,
                "Size": SEARCH_PAGE_SIZE,
                "EnableTopResults": false,
            }],
            "Cvid": super::uuid(),
            "Scenario": {
                "Name": "teams_search"
            }
        });
        if !scope_filter.is_empty() {
            request_obj["EntityRequests"][0]["Filter"] = Value::Object(scope_filter);
        }

        let endpoint = self.search_endpoint()?;
        let answer = self.call("POST", &endpoint, Auth::Bearer(Audience::Substrate), Body::Json(&request_obj), true)?.body;
        let me = self.me();
        let space_group = match scope {
            SearchScope::Space(space_id) => self.ctx.note(&format!("group:{space_id}")),
            _ => None,
        };

        let raw_hits = extract_raw_hits(&answer);
        let mut hits = Vec::new();
        for raw in raw_hits {
            let Some(parsed) = parse_hit(raw, &me) else {
                continue;
            };
            if !matches_filters(&parsed, filters) {
                continue;
            }
            match scope {
                SearchScope::Conversation(id) if thread_of(id) != parsed.thread_id => continue,
                SearchScope::Space(space_id) => {
                    let in_space = parsed.team_id == *space_id
                        || space_group
                            .as_deref()
                            .is_some_and(|g| !g.is_empty() && parsed.team_id.eq_ignore_ascii_case(g))
                        || parsed.thread_id == *space_id
                        || self
                            .ctx
                            .known(&parsed.thread_id)
                            .ok()
                            .flatten()
                            .and_then(|row| row.space)
                            .is_some_and(|s| s == *space_id);
                    if !in_space {
                        continue;
                    }
                }
                SearchScope::Account | SearchScope::Conversation(_) => {}
            }

            // Ensure the target conversation (or reply chain) exists in the store so jumping
            // to a hit outside the local cache can immediately load `HistoryAround`.
            let _ = self.known(&parsed.thread_id);
            let is_channel = super::tree::is_channel(&parsed.thread_id);
            let place = match parsed.parent_id.as_deref() {
                Some(root) if is_channel => {
                    let _ = self.chain_known(&parsed.thread_id, root);
                    chain_id(&parsed.thread_id, root)
                }
                _ => parsed.thread_id.clone(),
            };

            let known = self.ctx.known(&place).ok().flatten().or_else(|| self.ctx.known(&parsed.thread_id).ok().flatten());
            let title = known
                .as_ref()
                .map(|r| r.title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| parsed.title.clone());
            let space_title = known
                .as_ref()
                .map(|r| r.space_title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| parsed.space_title.clone());
            let sender = if !parsed.sender_name.is_empty() {
                parsed.sender_name.clone()
            } else if !parsed.sender_id.is_empty() {
                self.name_of(&parsed.sender_id).unwrap_or_else(|| parsed.sender_id.clone())
            } else {
                String::new()
            };
            let snippet = crate::store::snippet(&parsed.text, query.trim());
            let outgoing = !me.is_empty() && parsed.sender_id == me;

            hits.push(MessageHit {
                conversation: self.ctx.key(&place),
                message_id: parsed.message_id,
                sent: parsed.sent,
                sender,
                snippet,
                title,
                space_title,
                outgoing,
            });
        }

        Ok(hits)
    }

    fn search_endpoint(&self) -> Result<String, Fault> {
        let place = self.place()?;
        if let Some(ref url) = place.search {
            let base = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/');
            if base.ends_with("/query") {
                return Ok(url.clone());
            }
            return Ok(format!("{base}/v2/query"));
        }
        if let Some(ref moved) = self.moved {
            return Ok(format!("{moved}/msgsearch/v2/query"));
        }
        Ok(format!("{SUBSTRATE}/search/api/v2/query"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Key, Network};

    #[test]
    fn query_string_combines_text_and_structured_filters() {
        let acct = AccountKey { network: Network::Teams, account: "t1".into() };
        let filters = SearchFilters {
            from: Some(Key::new(&acct, "8:orgid:ana")),
            has: Some(SearchHas::Files),
            before: Some(1_760_000_500),
            after: Some(1_760_000_100),
            mentions_me: true,
            ..SearchFilters::default()
        };
        let q = build_query_string("deploy plan", Some("ana@contoso.com"), &filters);
        assert!(q.contains("deploy plan"), "{q}");
        assert!(q.contains("from:\"ana@contoso.com\""), "{q}");
        assert!(q.contains("hasAttachment:true"), "{q}");
        assert!(q.contains("mentionsMe:true"), "{q}");
        assert!(q.contains("sent>="), "{q}");
        assert!(q.contains("sent<="), "{q}");

        // Multi-word display names stay quoted, and quotes/backslashes/control chars cannot inject KQL.
        let multi = build_query_string("budget\r\n", Some("Ana \"Admin\"\\Ruiz\n"), &SearchFilters::default());
        assert_eq!(multi, "budget from:\"Ana AdminRuiz\"");
    }

    #[test]
    fn substrate_hits_and_highlights_parse_cleanly() {
        let payload = json!({
            "EntitySets": [{
                "ResultSets": [{
                    "Results": [{
                        "HitHighlightedSummary": "Check the <c0>deploy</c0> runbook<ddd/>",
                        "Source": {
                            "ThreadId": "19:platform0general@thread.tacv2",
                            "MessageId": "1760000210210",
                            "ParentMessageId": "1760000200200",
                            "TeamId": "19:platform0general@thread.tacv2",
                            "TeamName": "Platform",
                            "ChannelName": "General",
                            "From": {"Mri": "8:orgid:ana", "Name": "Ana Ruiz"},
                            "HasAttachments": true,
                            "MentionsMe": true
                        }
                    }]
                }]
            }]
        });
        let raw = extract_raw_hits(&payload);
        assert_eq!(raw.len(), 1);
        let hit = parse_hit(raw[0], "8:orgid:me").expect("parsed hit");
        assert_eq!(hit.thread_id, "19:platform0general@thread.tacv2");
        assert_eq!(hit.message_id, "1760000210210");
        assert_eq!(hit.parent_id.as_deref(), Some("1760000200200"));
        assert_eq!(hit.text, "Check the deploy runbook…");
        assert_eq!(hit.has_files, Some(true));
        assert_eq!(hit.mentions_me, Some(true));

        // Server hits that omit optional filter fields (HasAttachments, MentionsMe) are not dropped.
        let sparse = json!({
            "HitHighlightedSummary": "Server matched this hit",
            "Source": {
                "ThreadId": "19:platform0general@thread.tacv2",
                "MessageId": "1760000210299"
            }
        });
        let sparse_hit = parse_hit(&sparse, "8:orgid:me").expect("sparse hit");
        assert_eq!(sparse_hit.has_files, None);
        assert_eq!(sparse_hit.mentions_me, None);
        assert!(matches_filters(
            &sparse_hit,
            &SearchFilters {
                has: Some(SearchHas::Files),
                mentions_me: true,
                ..Default::default()
            }
        ));
    }
}
