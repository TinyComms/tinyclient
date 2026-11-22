//! Teams team tags (`@On-Call`, `@Design`, …): fetching a team's tags and members from the
//! middle tier (with Microsoft Graph fallback), remembering which tags belong to the signed-in
//! user for `mentions_me`, and starting a group chat with a tag's members.

use super::{escaped, Auth, Body, Fault, Teams, GRAPH, SPACES};
use protocol::{Key, ProfilePerson, TeamTag};
use serde_json::Value;

/// Parses tags from either Teams middle-tier (`/beta/teams/{id}/tags`) or Microsoft Graph
/// (`/v1.0/teams/{groupId}/tags?$expand=members`) JSON payloads.
pub(super) fn parse_tags(
    raw: &Value,
    me: &str,
    make_key: impl Fn(&str) -> Key,
    lookup_name: impl Fn(&str) -> Option<(String, String)>,
) -> Vec<TeamTag> {
    let list = raw
        .as_array()
        .or_else(|| raw["tags"].as_array())
        .or_else(|| raw["value"].as_array())
        .map_or(&[][..], Vec::as_slice);
    let mut tags = Vec::new();
    for entry in list {
        let id = entry["id"].as_str().unwrap_or("").trim();
        let name = entry["displayName"]
            .as_str()
            .or_else(|| entry["name"].as_str())
            .unwrap_or("")
            .trim();
        if id.is_empty() || name.is_empty() {
            continue;
        }
        let description = entry["description"].as_str().unwrap_or("").trim().to_owned();
        let mut members = Vec::new();
        let mut mine = entry["isMember"] == true || entry["membership"] == true;
        for m in entry["members"].as_array().into_iter().flatten() {
            let mri = m["mri"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    m["userId"]
                        .as_str()
                        .or_else(|| m["id"].as_str())
                        .filter(|s| !s.is_empty())
                        .map(|oid| if oid.starts_with("8:") { oid.to_owned() } else { format!("8:orgid:{oid}") })
                });
            let Some(mri) = mri else {
                continue;
            };
            if !me.is_empty() && mri == me {
                mine = true;
            }
            let (cached_name, cached_title) = lookup_name(&mri).unwrap_or_default();
            let member_name = m["displayName"]
                .as_str()
                .or_else(|| m["name"].as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| if cached_name.is_empty() { mri.clone() } else { cached_name });
            let title = m["jobTitle"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or(cached_title);
            if !members.iter().any(|existing: &ProfilePerson| existing.key.id == mri) {
                members.push(ProfilePerson {
                    key: make_key(&mri),
                    name: member_name,
                    title,
                });
            }
        }
        let declared = entry["memberCount"].as_u64().unwrap_or(0) as u32;
        let member_count = declared.max(members.len() as u32);
        tags.push(TeamTag {
            id: id.to_owned(),
            name: name.to_owned(),
            description,
            member_count,
            members,
            mine,
        });
    }
    tags.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    tags
}

impl Teams {
    /// Returns true if `tag_id` is a tag the signed-in user belongs to.
    pub(super) fn is_my_tag(&self, tag_id: &str) -> bool {
        if tag_id.is_empty() {
            return false;
        }
        let mut state = self.state();
        if state.my_tags.is_empty() {
            if let Some(raw) = self.ctx.note("my_tags").filter(|s| !s.is_empty()) {
                if let Ok(ids) = serde_json::from_str::<Vec<String>>(&raw) {
                    state.my_tags.extend(ids);
                }
            }
        }
        state.my_tags.contains(tag_id)
    }

    /// Fetches all mentionable tags (and their members) for a team `space`, trying Teams'
    /// middle-tier tags route first and falling back to Microsoft Graph.
    pub(super) fn space_tags(&self, space: &str) -> Result<Vec<TeamTag>, Fault> {
        let place = self.place()?;
        let mt_url = format!("{}/beta/teams/{}/tags", place.middle, escaped(space));
        let raw = match self.call("GET", &mt_url, Auth::Both(SPACES), Body::None, true) {
            Ok(answer) => answer.body,
            Err(_) => {
                let Some(group_id) = self.ctx.note(&format!("group:{space}")).filter(|s| !s.is_empty()) else {
                    return Ok(Vec::new());
                };
                let graph_url = format!("{}/v1.0/teams/{}/tags?$expand=members", self.graph, escaped(&group_id));
                match self.call("GET", &graph_url, Auth::Bearer(GRAPH), Body::None, true) {
                    Ok(answer) => answer.body,
                    Err(_) => return Ok(Vec::new()),
                }
            }
        };
        let me = self.me();
        let initial = parse_tags(
            &raw,
            &me,
            |mri| self.ctx.key(mri),
            |mri| self.cached(mri).map(|k| (k.name, k.title)),
        );
        let unknown: Vec<String> = initial
            .iter()
            .flat_map(|t| t.members.iter())
            .filter(|m| m.name == m.key.id)
            .map(|m| m.key.id.clone())
            .collect();
        if !unknown.is_empty() {
            self.lookup(&unknown);
        }
        let tags = if unknown.is_empty() {
            initial
        } else {
            parse_tags(
                &raw,
                &me,
                |mri| self.ctx.key(mri),
                |mri| self.cached(mri).map(|k| (k.name, k.title)),
            )
        };
        let persisted_mine: Vec<String> = {
            let mut state = self.state();
            if state.my_tags.is_empty() {
                if let Some(raw) = self.ctx.note("my_tags").filter(|s| !s.is_empty()) {
                    if let Ok(ids) = serde_json::from_str::<Vec<String>>(&raw) {
                        state.my_tags.extend(ids);
                    }
                }
            }
            // Only reconcile tags that belong to this team so persisted `my_tags` from other
            // teams are never wiped before those teams are fetched.
            if let Some(previous) = state.tags.insert(space.to_owned(), tags.clone()) {
                for old in previous {
                    state.my_tags.remove(&old.id);
                }
            }
            for tag in &tags {
                if tag.mine {
                    state.my_tags.insert(tag.id.clone());
                } else {
                    state.my_tags.remove(&tag.id);
                }
            }
            let mut sorted: Vec<String> = state.my_tags.iter().cloned().collect();
            sorted.sort();
            sorted
        };
        self.ctx.set_note("my_tags", &serde_json::to_string(&persisted_mine).unwrap_or_default());
        Ok(tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Network;
    use serde_json::json;

    fn key(id: &str) -> Key {
        Key {
            network: Network::Teams,
            account: "t1".into(),
            id: id.into(),
        }
    }

    #[test]
    fn parses_middle_tier_and_graph_team_tags() {
        let mt = json!({
            "tags": [
                {
                    "id": "tag-oncall",
                    "displayName": "On-Call",
                    "description": "Primary incident responders",
                    "memberCount": 2,
                    "members": [
                        {"mri": "8:orgid:me", "displayName": "Me Myself", "jobTitle": "Engineer"},
                        {"mri": "8:orgid:ana", "displayName": "Ana Ruiz", "jobTitle": "Staff Engineer"}
                    ]
                },
                {
                    "id": "tag-design",
                    "displayName": "Design",
                    "memberCount": 1,
                    "members": [{"mri": "8:orgid:cy", "displayName": "Cy Tran"}]
                }
            ]
        });
        let tags = parse_tags(&mt, "8:orgid:me", key, |_| None);
        assert_eq!(tags.len(), 2);
        // Sorted alphabetically: Design, On-Call.
        assert_eq!(tags[0].name, "Design");
        assert!(!tags[0].mine);
        assert_eq!(tags[1].name, "On-Call");
        assert!(tags[1].mine);
        assert_eq!(tags[1].member_count, 2);
        assert_eq!(tags[1].members.len(), 2);

        let graph = json!({
            "value": [{
                "id": "g-tag-1",
                "displayName": "SRE",
                "memberCount": 1,
                "members": [{"userId": "me", "displayName": "Me Myself"}]
            }]
        });
        let gtags = parse_tags(&graph, "8:orgid:me", key, |_| None);
        assert_eq!(gtags.len(), 1);
        assert!(gtags[0].mine);
        assert_eq!(gtags[0].members[0].key.id, "8:orgid:me");
    }
}
