//! Ephemeral stories (Telegram #540): per-person unseen ring summaries, individual story items,
//! and forwarded/shared stories attached to chat messages.

use crate::{is_false, is_zero, Key, MediaFile, Span};
use serde::{Deserialize, Serialize};

/// What kind of media a story holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryMediaKind {
    Photo,
    Video,
    #[default]
    Unsupported,
}

/// Summary of one peer's active stories, used to draw the unseen-story ring on avatars in
/// People and chat headers without prefetching story contents or marking anything seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerStoryRing {
    pub peer: Key,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author_name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unseen: u32,
    #[serde(default)]
    pub max_id: i32,
    #[serde(default)]
    pub max_read_id: i32,
    /// True when any active unseen story from this peer is marked Close Friends.
    #[serde(default, skip_serializing_if = "is_false")]
    pub close_friends: bool,
}

impl PeerStoryRing {
    pub fn has_unseen(&self) -> bool {
        self.unseen > 0
    }
}

/// One active story item returned for a peer (`Request::Stories`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Story {
    pub id: String,
    pub peer: Key,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author_name: String,
    /// Unix seconds when the story was posted.
    pub posted_at: i64,
    /// Unix seconds when the story expires.
    pub expires_at: i64,
    /// True once the user has opened/read this story (never set by prefetch).
    #[serde(default, skip_serializing_if = "is_false")]
    pub seen: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub close_friends: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
    /// True when this story was posted by the signed-in account.
    #[serde(default, skip_serializing_if = "is_false")]
    pub own: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub noforwards: bool,
    #[serde(default)]
    pub media_kind: StoryMediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<MediaFile>,
    /// Video duration in milliseconds when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caption: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caption_spans: Vec<Span>,
    /// Our reaction emoji (or custom emoji id) on this story, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_reaction: Option<String>,
    /// Total viewer count reported on our own stories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub views_count: Option<u32>,
    /// Total reaction count reported on our own stories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions_count: Option<u32>,
}

/// A forwarded or shared story embedded in a chat message (`MessageMedia::Story`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedStory {
    pub peer: Key,
    pub story_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caption: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caption_spans: Vec<Span>,
    #[serde(default)]
    pub media_kind: StoryMediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub expired: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub via_mention: bool,
}

impl SharedStory {
    pub fn bound(&mut self) {
        if self.author_name.len() > 256 {
            let mut end = 256;
            while !self.author_name.is_char_boundary(end) {
                end -= 1;
            }
            self.author_name.truncate(end);
        }
        if self.caption.len() > 2_000 {
            let mut end = 2_000;
            while !self.caption.is_char_boundary(end) {
                end -= 1;
            }
            self.caption.truncate(end);
        }
        let kept = self.caption.len();
        self.caption_spans
            .retain(|span| span.start <= span.end && span.end as usize <= kept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Network;

    #[test]
    fn story_and_ring_round_trip() {
        let peer = Key {
            network: Network::Telegram,
            account: "7001".into(),
            id: "u7002".into(),
        };
        let ring = PeerStoryRing {
            peer: peer.clone(),
            author_name: "Nadia".into(),
            total: 2,
            unseen: 1,
            max_id: 102,
            max_read_id: 101,
            close_friends: true,
        };
        assert!(ring.has_unseen());
        let encoded = serde_json::to_string(&ring).unwrap();
        let decoded: PeerStoryRing = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, ring);

        let story = Story {
            id: "102".into(),
            peer: peer.clone(),
            author_name: "Nadia".into(),
            posted_at: 1_700_000_000,
            expires_at: 1_700_086_400,
            seen: false,
            close_friends: true,
            pinned: false,
            own: false,
            noforwards: false,
            media_kind: StoryMediaKind::Photo,
            media: None,
            thumb: None,
            duration_ms: None,
            caption: "Summit sunrise".into(),
            caption_spans: vec![],
            my_reaction: Some("❤️".into()),
            views_count: None,
            reactions_count: None,
        };
        let s_json = serde_json::to_string(&story).unwrap();
        let s_back: Story = serde_json::from_str(&s_json).unwrap();
        assert_eq!(s_back, story);

        let mut shared = SharedStory {
            peer,
            story_id: "102".into(),
            author_name: "Nadia".into(),
            caption: "Look at this".into(),
            caption_spans: vec![],
            media_kind: StoryMediaKind::Photo,
            media: None,
            thumb: None,
            expired: false,
            via_mention: false,
        };
        shared.bound();
        let sh_json = serde_json::to_string(&shared).unwrap();
        let sh_back: SharedStory = serde_json::from_str(&sh_json).unwrap();
        assert_eq!(sh_back, shared);
    }
}
