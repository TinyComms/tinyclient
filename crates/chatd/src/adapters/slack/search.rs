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
    pub(super) fn search_messages(&self, query: &str, scope: &SearchScope) -> Result<Vec<MessageHit>, Fault> {
        let capped: String = query.chars().take(SEARCH_MAX_CHARS).collect();
        let query = capped.trim();
        let (asked, only) = self.search_query(query, scope);
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
                snippet: crate::store::snippet(&rich.text, query),
                title,
                space_title: known.space_title.clone(),
                outgoing: !me.is_empty() && found["user"].as_str() == Some(me),
            });
        }
        Ok(hits)
    }

    /// Slack's own words for a scope: `in:#channel`, its own modifier, when one conversation
    /// is asked about. A direct conversation has no channel name; its hits are filtered by id.
    fn search_query(&self, query: &str, scope: &SearchScope) -> (String, Option<String>) {
        let SearchScope::Conversation(id) = scope else {
            return (query.to_owned(), None);
        };
        let channel = thread_of(id).map_or(id.as_str(), |(channel, _)| channel);
        let filter = self.ctx.known(channel).ok().flatten().and_then(|known| {
            if known.title.starts_with('#') {
                return Some(known.title);
            }
            // Slack names a direct conversation's partner `@handle` in a search.
            self.state().partners.iter().find(|(_, place)| place.as_str() == channel).and_then(|(person, _)| {
                self.cached(person).map(|user| format!("@{}", user.handle)).filter(|handle| handle.len() > 1)
            })
        });
        match filter {
            Some(filter) => (format!("{query} in:{filter}"), Some(channel.to_owned())),
            None => (query.to_owned(), Some(channel.to_owned())),
        }
    }
}
