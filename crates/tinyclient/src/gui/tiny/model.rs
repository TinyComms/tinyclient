//! What the server page shows, worked out from the community chatd read, with nothing drawn:
//! what this account may do, the channel list as rows, the member list in its groups, and the
//! person menu's items. Every rule that hides a control is here, so it is tested here.
//!
//! On a server that says nothing of its capabilities (an older tinyserver), every check falls
//! back to the saved role, as the page always did (`TinyCommunity::allowed` and `outranks`), and
//! nothing that needs a newer server is offered.

use protocol::{
    TinyCommunity, TinyLayoutGroup, TinyMember, TinyRole, TinyRoom, TinySeat, TINY_FEATURE_BANS, TINY_FEATURE_CATEGORIES, TINY_FEATURE_INVITE_OPTIONS, TINY_FEATURE_ROLES,
    TINY_FEATURE_SERVER, TINY_FEATURE_TIMEOUTS, TINY_FEATURE_VOICE_MODERATION, TINY_DEAFEN_MEMBERS, TINY_DISCONNECT_MEMBERS, TINY_MOVE_MEMBERS,
    TINY_MUTE_MEMBERS,
};
use std::collections::HashSet;

/// What this account may do on the server, as a whole.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct May {
    pub invite: bool,
    /// An invite can carry roles, a label, a use limit and an expiry.
    pub invite_options: bool,
    /// Roles may be put on an invite.
    pub invite_roles: bool,
    /// Never-expiring invites: owners and manage_server only.
    pub invite_forever: bool,
    pub read_invites: bool,
    pub make_room: bool,
    pub edit_room: bool,
    pub delete_room: bool,
    pub categories: bool,
    /// Rooms and categories can be dragged into a new order.
    pub layout: bool,
    pub edit_server: bool,
    pub manage_roles: bool,
    pub read_audit: bool,
    pub read_bans: bool,
    /// Email addresses are shown: what the server sends to those who manage people.
    pub see_email: bool,
    pub owner: bool,
}

pub fn may(community: &TinyCommunity) -> May {
    let can = |id: &str| community.allowed(id);
    let has = |feature: &str| community.has(feature);
    let owner = community.access.as_ref().map_or(community.role.eq_ignore_ascii_case("owner"), |access| access.owner);
    let manage_server = community.access.as_ref().is_some_and(|access| access.has(protocol::TINY_MANAGE_SERVER));
    May {
        invite: can("community.invites.create"),
        invite_options: can("community.invites.create") && has(TINY_FEATURE_INVITE_OPTIONS),
        invite_roles: can("community.invites.roles") && has(TINY_FEATURE_INVITE_OPTIONS) && has(TINY_FEATURE_ROLES),
        invite_forever: has(TINY_FEATURE_INVITE_OPTIONS) && (owner || manage_server),
        read_invites: can("community.invites.read"),
        make_room: can("community.rooms.create"),
        edit_room: can("community.rooms.rename"),
        delete_room: can("community.rooms.delete"),
        categories: has(TINY_FEATURE_CATEGORIES) && can("community.categories.manage"),
        layout: has(TINY_FEATURE_CATEGORIES) && can("community.categories.manage"),
        edit_server: has(TINY_FEATURE_SERVER) && can("community.server.edit"),
        manage_roles: can("community.members.role"),
        read_audit: can("community.audit.read"),
        read_bans: has(TINY_FEATURE_BANS) && can("community.bans.read"),
        see_email: can("community.members.remove") || can("community.members.role"),
        owner,
    }
}

/// A name to show for someone, and their email when that is all there is to go by.
pub fn name_of(member: &TinyMember) -> String {
    if !member.display_name.trim().is_empty() {
        member.display_name.clone()
    } else if !member.email.is_empty() {
        member.email.clone()
    } else {
        "Someone".to_owned()
    }
}

pub fn name_by_id(community: &TinyCommunity, user_id: &str) -> String {
    community.member(user_id).map_or_else(|| "Someone".to_owned(), name_of)
}

/// The server's name: its own when it has one, else the host it is on.
pub fn server_name(community: &TinyCommunity, account_name: &str) -> String {
    community
        .server
        .as_ref()
        .map(|server| server.name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| Some(account_name.trim().to_owned()).filter(|name| !name.is_empty()))
        .unwrap_or_else(|| {
            let host = protocol::join_host(&community.origin);
            if host.is_empty() { "Tiny server".to_owned() } else { host }
        })
}

/// A member's roles, most senior first, `@everyone` left out.
pub fn roles_of<'a>(community: &'a TinyCommunity, member: &TinyMember) -> Vec<&'a TinyRole> {
    let mut roles: Vec<&TinyRole> = community.roles.iter().filter(|role| !role.default && member.roles.contains(&role.id)).collect();
    roles.sort_by_key(|role| std::cmp::Reverse(role.position));
    roles
}

/// The colour a member's name is drawn in: their most senior role that has one.
pub fn color_of(community: &TinyCommunity, member: &TinyMember) -> Option<u32> {
    roles_of(community, member).into_iter().find_map(|role| role.color.as_deref().and_then(hex))
}

/// `#RRGGBB` as a number, for `gpui::rgb`.
pub fn hex(color: &str) -> Option<u32> {
    let digits = color.trim().strip_prefix('#')?;
    (digits.len() == 6).then(|| u32::from_str_radix(digits, 16).ok()).flatten()
}

/// What a legacy server calls a role, in words.
pub fn role_word(role: &str) -> &'static str {
    match role.to_ascii_lowercase().as_str() {
        "owner" => "Owner",
        "admin" => "Admin",
        _ => "Member",
    }
}

pub fn is_owner(member: &TinyMember) -> bool {
    member.owner || member.role.eq_ignore_ascii_case("owner")
}

/// The room someone's seat is in, and the seat.
pub fn seat_of<'a>(community: &'a TinyCommunity, user_id: &str) -> Option<(&'a TinyRoom, &'a TinySeat)> {
    community.rooms.iter().find_map(|room| room.participants.iter().find(|seat| seat.user_id == user_id).map(|seat| (room, seat)))
}

/// `3 / 12`, or `3` where the room has no limit of its own.
pub fn occupancy(room: &TinyRoom) -> String {
    match room.user_limit {
        0 => room.participants.len().to_string(),
        limit => format!("{} / {limit}", room.participants.len()),
    }
}

pub fn full(room: &TinyRoom) -> bool {
    room.user_limit > 0 && room.participants.len() >= room.user_limit as usize
}

/// Words for what is true of a seat beyond speaking, most serious first.
pub fn seat_states(seat: &TinySeat) -> Vec<&'static str> {
    let mut states = Vec::new();
    if seat.server_deafened {
        states.push("Server deafened");
    } else if seat.deafened {
        states.push("Deafened");
    }
    if seat.server_muted {
        states.push("Server muted");
    } else if seat.suppressed {
        states.push("Can't speak here");
    } else if seat.muted && !seat.deafened {
        states.push("Muted");
    }
    if seat.priority {
        states.push("Priority speaker");
    }
    states
}

/// One row of the channel list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A category's header. `None` is the rooms in no category, headed only when there are
    /// categories too. `key` is what folding remembers.
    Category { id: Option<String>, name: String, folded: bool, key: String, rooms: usize },
    Room { id: String },
    Seat { room: String, user: String },
}

/// The key under which an uncategorized group folds.
pub const LOOSE: &str = "";

/// The channel list in order: rooms in no category first, then each category by its place,
/// each room by its place and then name; under a room, who is in its call. A folded category
/// keeps a room someone is in visible only when it is ours, as Discord keeps the connected one.
pub fn lines(community: &TinyCommunity, folded: &HashSet<String>, mine: Option<&str>) -> Vec<Line> {
    let mut rooms: Vec<&TinyRoom> = community.rooms.iter().collect();
    rooms.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let mut categories: Vec<_> = community.categories.iter().collect();
    categories.sort_by_key(|category| category.position);
    let known = |id: &Option<String>| id.as_ref().is_some_and(|id| categories.iter().any(|category| &category.id == id));
    let mut out = Vec::new();
    let mut group = |id: Option<String>, name: String, members: Vec<&TinyRoom>, headed: bool| {
        let key = id.clone().unwrap_or_else(|| LOOSE.to_owned());
        let is_folded = folded.contains(&key);
        if headed {
            out.push(Line::Category { id: id.clone(), name, folded: is_folded, key, rooms: members.len() });
        }
        for room in members {
            let ours = mine.is_some_and(|me| room.participants.iter().any(|seat| seat.user_id == me));
            if is_folded && !ours {
                continue;
            }
            out.push(Line::Room { id: room.id.clone() });
            for seat in &room.participants {
                out.push(Line::Seat { room: room.id.clone(), user: seat.user_id.clone() });
            }
        }
    };
    let loose: Vec<&TinyRoom> = rooms.iter().copied().filter(|room| !known(&room.category_id)).collect();
    if categories.is_empty() {
        // An older server has no categories: one header over all of its rooms.
        group(None, "Voice channels".to_owned(), loose, true);
        return out;
    }
    if !loose.is_empty() {
        group(None, String::new(), loose, false);
    }
    for category in categories {
        let inside: Vec<&TinyRoom> = rooms.iter().copied().filter(|room| room.category_id.as_deref() == Some(category.id.as_str())).collect();
        group(Some(category.id.clone()), category.name.clone(), inside, true);
    }
    out
}

/// The channel list's order as the server keeps it: the rooms in no category, then each
/// category's, top first. What `TinyLayout` sends, whole.
pub fn layout_of(community: &TinyCommunity) -> Vec<TinyLayoutGroup> {
    let order: Vec<String> = lines(community, &HashSet::new(), None)
        .into_iter()
        .filter_map(|line| match line {
            Line::Room { id } => Some(id),
            _ => None,
        })
        .collect();
    let mut categories: Vec<_> = community.categories.iter().collect();
    categories.sort_by_key(|category| category.position);
    let known = |id: &Option<String>| id.as_ref().is_some_and(|id| categories.iter().any(|category| &category.id == id));
    let in_group = |category: Option<&str>| -> Vec<String> {
        order
            .iter()
            .filter(|id| {
                community.rooms.iter().find(|room| &room.id == *id).is_some_and(|room| match category {
                    None => !known(&room.category_id),
                    Some(category) => room.category_id.as_deref() == Some(category),
                })
            })
            .cloned()
            .collect()
    };
    let mut groups = vec![TinyLayoutGroup { category_id: None, room_ids: in_group(None) }];
    groups.extend(categories.iter().map(|category| TinyLayoutGroup { category_id: Some(category.id.clone()), room_ids: in_group(Some(&category.id)) }));
    groups
}

/// Where a dragged room lands: just above `before`, or at the end of `category` (`None` is the
/// rooms in no category).
pub fn move_room(mut groups: Vec<TinyLayoutGroup>, room: &str, category: Option<&str>, before: Option<&str>) -> Vec<TinyLayoutGroup> {
    for group in &mut groups {
        group.room_ids.retain(|id| id != room);
    }
    if let Some(group) = groups.iter_mut().find(|group| group.category_id.as_deref() == category) {
        let at = before.and_then(|before| group.room_ids.iter().position(|id| id == before)).unwrap_or(group.room_ids.len());
        group.room_ids.insert(at, room.to_owned());
    }
    groups
}

/// A dragged category lands just above `before`. The rooms in no category stay first.
pub fn move_category(mut groups: Vec<TinyLayoutGroup>, category: &str, before: &str) -> Vec<TinyLayoutGroup> {
    let Some(from) = groups.iter().position(|group| group.category_id.as_deref() == Some(category)) else {
        return groups;
    };
    let moved = groups.remove(from);
    let at = groups.iter().position(|group| group.category_id.as_deref() == Some(before)).unwrap_or(groups.len()).max(1);
    groups.insert(at, moved);
    groups
}

/// The page in a new order, before the server confirms it.
pub fn apply_layout(community: &mut TinyCommunity, groups: &[TinyLayoutGroup]) {
    for group in groups {
        for (position, id) in group.room_ids.iter().enumerate() {
            if let Some(room) = community.rooms.iter_mut().find(|room| &room.id == id) {
                room.category_id = group.category_id.clone();
                room.position = position as i32;
            }
        }
    }
    for (position, id) in groups.iter().filter_map(|group| group.category_id.as_ref()).enumerate() {
        if let Some(category) = community.categories.iter_mut().find(|category| &category.id == id) {
            category.position = position as i32;
        }
    }
}

/// One group of the member list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    pub color: Option<u32>,
    pub people: Vec<String>,
    pub offline: bool,
}

/// The member list: online people under their most senior role shown separately (in its
/// colour), owners first, then everyone else online as Members, then Offline. Without a live
/// socket nobody's presence is known, so everyone is listed as online would be. `query`
/// narrows by name or email.
pub fn groups(community: &TinyCommunity, live: bool, query: &str) -> Vec<Group> {
    let query = query.trim().to_lowercase();
    let online: HashSet<&str> = community.online.iter().map(String::as_str).collect();
    let mut hoisted: Vec<&TinyRole> = community.roles.iter().filter(|role| role.hoist && !role.default).collect();
    hoisted.sort_by_key(|role| std::cmp::Reverse(role.position));
    let legacy = community.roles.is_empty();
    let mut owners = Vec::new();
    let mut by_role: Vec<Vec<String>> = vec![Vec::new(); hoisted.len()];
    let mut admins = Vec::new();
    let mut rest = Vec::new();
    let mut offline = Vec::new();
    let mut members: Vec<&TinyMember> = community.members.iter().collect();
    members.sort_by_key(|member| name_of(member).to_lowercase());
    for member in members {
        if !query.is_empty() && !name_of(member).to_lowercase().contains(&query) && !member.email.to_lowercase().contains(&query) {
            continue;
        }
        let id = member.user_id.clone();
        if live && !online.contains(member.user_id.as_str()) {
            offline.push(id);
            continue;
        }
        if is_owner(member) {
            owners.push(id);
        } else if legacy && member.role.eq_ignore_ascii_case("admin") {
            admins.push(id);
        } else if let Some(at) = hoisted.iter().position(|role| member.roles.contains(&role.id)) {
            by_role[at].push(id);
        } else {
            rest.push(id);
        }
    }
    let mut out = Vec::new();
    let mut push = |title: String, color: Option<u32>, people: Vec<String>, offline: bool| {
        if !people.is_empty() {
            out.push(Group { title, color, people, offline });
        }
    };
    push("Owner".to_owned(), None, owners, false);
    push("Admin".to_owned(), None, admins, false);
    for (role, people) in hoisted.iter().zip(by_role) {
        push(role.name.clone(), role.color.as_deref().and_then(hex), people, false);
    }
    push(if live { "Online".to_owned() } else { "Members".to_owned() }, None, rest, false);
    push("Offline".to_owned(), None, offline, true);
    out
}

/// A role in the person menu's Roles checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleChoice {
    pub id: String,
    pub name: String,
    pub color: Option<u32>,
    pub on: bool,
}

/// What the person menu offers for one person. Items absent here are absent from the menu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonMenu {
    pub me: bool,
    /// Volume and Mute for me: anyone but ourselves.
    pub local: bool,
    /// Where they sit, if they do.
    pub room: Option<String>,
    pub server_mute: Option<bool>,
    pub server_deafen: Option<bool>,
    /// Rooms they can be moved to.
    pub move_to: Vec<(String, String)>,
    pub disconnect: bool,
    pub timeout: bool,
    pub timed_out: bool,
    /// Roles on a server with roles.
    pub roles: Vec<RoleChoice>,
    /// Owner, Admin, Member on an older server: the one to change to, with whether it is theirs.
    pub legacy_roles: Vec<(&'static str, bool)>,
    pub kick: bool,
    pub ban: bool,
}

impl PersonMenu {
    pub fn moderates(&self) -> bool {
        self.server_mute.is_some() || self.server_deafen.is_some() || !self.move_to.is_empty() || self.disconnect || self.timeout
    }

    pub fn manages(&self) -> bool {
        !self.roles.is_empty() || !self.legacy_roles.is_empty() || self.kick || self.ban
    }
}

fn legacy_role_change(caller: &str, target: &str, next: &str, owners: usize) -> bool {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    if same(target, next) {
        return false;
    }
    if same(target, "owner") && owners <= 1 {
        return false;
    }
    if same(target, "owner") || same(next, "owner") {
        return same(caller, "owner");
    }
    if same(caller, "owner") {
        return true;
    }
    same(caller, "admin") && same(target, "member") && same(next, "admin")
}

/// The person menu for `user_id`, from where this account stands.
pub fn person_menu(community: &TinyCommunity, user_id: &str) -> PersonMenu {
    let Some(target) = community.member(user_id) else {
        return PersonMenu::default();
    };
    let me = user_id == community.me;
    let above = community.outranks(target);
    let can = |id: &str| community.allowed(id);
    let seated = seat_of(community, user_id);
    let moderation = community.has(TINY_FEATURE_VOICE_MODERATION);
    let in_room = |room: &TinyRoom, bits: u64| community.access.as_ref().is_some_and(|access| access.has_in(room, bits));
    let mut menu = PersonMenu { me, local: !me, room: seated.map(|(room, _)| room.id.clone()), ..PersonMenu::default() };
    if let Some((room, seat)) = seated.filter(|_| above) {
        if moderation && in_room(room, TINY_MUTE_MEMBERS) {
            menu.server_mute = Some(seat.server_muted);
        }
        if moderation && in_room(room, TINY_DEAFEN_MEMBERS) {
            menu.server_deafen = Some(seat.server_deafened);
        }
        if moderation && in_room(room, TINY_MOVE_MEMBERS) {
            menu.move_to = community
                .rooms
                .iter()
                .filter(|other| other.id != room.id && in_room(other, TINY_MOVE_MEMBERS))
                .map(|other| (other.id.clone(), other.name.clone()))
                .collect();
        }
        menu.disconnect = match &community.access {
            Some(_) => in_room(room, TINY_DISCONNECT_MEMBERS),
            None => can("community.calls.kick"),
        };
    }
    if above {
        menu.timeout = community.has(TINY_FEATURE_TIMEOUTS) && can("community.members.timeout");
        menu.timed_out = target.timeout_until.is_some();
        menu.kick = can("community.members.remove");
        menu.ban = community.has(TINY_FEATURE_BANS) && can("community.members.ban");
        if community.has(TINY_FEATURE_ROLES) && can("community.members.role") {
            let mut roles: Vec<&TinyRole> = community.roles.iter().filter(|role| !role.default).collect();
            roles.sort_by_key(|role| std::cmp::Reverse(role.position));
            menu.roles = roles
                .into_iter()
                .filter(|role| community.access.as_ref().is_some_and(|access| access.may_assign(role)))
                .map(|role| RoleChoice { id: role.id.clone(), name: role.name.clone(), color: role.color.as_deref().and_then(hex), on: target.roles.contains(&role.id) })
                .collect();
        }
    }
    if community.access.is_none() && can("community.members.role") && !me {
        let owners = community.members.iter().filter(|member| is_owner(member)).count();
        menu.legacy_roles = ["owner", "admin", "member"]
            .into_iter()
            .filter(|next| legacy_role_change(&community.role, &target.role, next, owners) || target.role.eq_ignore_ascii_case(next))
            .map(|next| (next, target.role.eq_ignore_ascii_case(next)))
            .collect();
        if menu.legacy_roles.iter().all(|(_, on)| *on) {
            menu.legacy_roles.clear();
        }
        // The only owner stays, whoever asks.
        if is_owner(target) && owners <= 1 {
            menu.kick = false;
        }
    }
    menu
}

/// The timeouts the menu offers, in seconds.
pub const TIMEOUTS: [(&str, i64); 6] = [("60 seconds", 60), ("5 minutes", 300), ("10 minutes", 600), ("1 hour", 3_600), ("1 day", 86_400), ("1 week", 604_800)];

/// Invite use limits: `None` is no limit.
pub const USES: [(&str, Option<u32>); 5] = [("1", Some(1)), ("5", Some(5)), ("10", Some(10)), ("25", Some(25)), ("No limit", None)];

/// Invite expiries in seconds: `None` is never.
pub const EXPIRIES: [(&str, Option<u32>); 5] = [("30 min", Some(1_800)), ("1 hour", Some(3_600)), ("1 day", Some(86_400)), ("7 days", Some(604_800)), ("Never", None)];

/// Ban lengths in days: `None` is for good.
pub const BANS: [(&str, Option<u32>); 4] = [("1 day", Some(1)), ("7 days", Some(7)), ("30 days", Some(30)), ("For good", None)];

/// What an invite does, in words, for the page that shows its code.
pub fn invite_terms(uses: Option<u32>, expiry: Option<u32>) -> String {
    let uses = match uses {
        Some(1) => "once".to_owned(),
        Some(n) => format!("{n} times"),
        None => "any number of times".to_owned(),
    };
    let expiry = match expiry {
        None => "never expires".to_owned(),
        Some(secs) => format!("for {}", span_words(i64::from(secs))),
    };
    format!("It works {uses}, {expiry}, and will not be shown again.")
}

/// `30 minutes`, `1 hour`, `7 days`.
pub fn span_words(secs: i64) -> String {
    let plural = |n: i64, one: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") };
    match secs {
        s if s % 86_400 == 0 => plural(s / 86_400, "day"),
        s if s % 3_600 == 0 => plural(s / 3_600, "hour"),
        s if s % 60 == 0 => plural(s / 60, "minute"),
        s => plural(s, "second"),
    }
}

/// How long ago, or how far ahead, in a few words: `just now`, `5 min ago`, `in 3 days`.
pub fn relative(now: i64, then: i64) -> String {
    let delta = now - then;
    let (ahead, gap) = (delta < 0, delta.abs());
    let words = match gap {
        0..=59 => return if ahead { "in a moment".to_owned() } else { "just now".to_owned() },
        60..=3_599 => format!("{} min", gap / 60),
        3_600..=86_399 => format!("{} h", gap / 3_600),
        _ => {
            let days = gap / 86_400;
            if days == 1 { "1 day".to_owned() } else { format!("{days} days") }
        }
    };
    if ahead { format!("in {words}") } else { format!("{words} ago") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{TinyAccess, TinyCapabilities, TinyCategory, TINY_ALL, TINY_EVERYONE_DEFAULT};

    fn member(id: &str, name: &str, role: &str, roles: &[&str]) -> TinyMember {
        TinyMember {
            user_id: id.into(),
            display_name: name.into(),
            role: role.into(),
            owner: role == "owner",
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            ..TinyMember::default()
        }
    }

    fn role(id: &str, name: &str, position: i32, hoist: bool) -> TinyRole {
        TinyRole { id: id.into(), name: name.into(), color: Some("#3FBF7A".into()), position, permissions: TINY_ALL, hoist, default: position == 0, member_count: 0 }
    }

    fn seated(room: &str, people: &[&str]) -> TinyRoom {
        TinyRoom { participants: people.iter().map(|id| TinySeat { user_id: (*id).into(), ..TinySeat::default() }).collect(), ..TinyRoom::new(room, room) }
    }

    fn legacy(role: &str) -> TinyCommunity {
        TinyCommunity {
            role: role.into(),
            members: vec![member("own", "Olive", "owner", &[]), member("me", "Robin", role, &[]), member("adm", "Ada", "admin", &[]), member("mem", "Mo", "member", &[])],
            invites: vec![],
            audit: vec![],
            origin: "https://tiny.example".into(),
            rooms: vec![seated("lounge", &["mem", "me"]), seated("games", &[])],
            online: vec![],
            media: "up".into(),
            capabilities: TinyCapabilities::default(),
            access: None,
            me: "me".into(),
            server: None,
            roles: vec![],
            categories: vec![],
            bans: vec![],
        }
    }

    fn modern() -> TinyCommunity {
        let mut community = legacy("admin");
        community.capabilities = TinyCapabilities {
            version: "1".into(),
            features: ["server", "roles", "categories", "bans", "timeouts", "invite_options", "audit_query", "voice_moderation"].into_iter().map(str::to_owned).collect(),
        };
        community.roles = vec![role("admin", "Admin", 3, true), role("mod", "Moderator", 2, true), role("crew", "Crew", 1, false), role("every", "@everyone", 0, false)];
        community.members = vec![
            member("own", "Olive", "owner", &[]),
            member("me", "Robin", "admin", &["admin"]),
            member("peer", "Pat", "admin", &["admin"]),
            member("mod", "Max", "member", &["mod", "crew"]),
            member("mem", "Mo", "member", &[]),
        ];
        community.access = Some(TinyAccess { owner: false, roles: vec!["admin".into()], permissions: TINY_ALL, rank: 3 });
        community.categories = vec![TinyCategory { id: "talk".into(), name: "Talk".into(), position: 1 }, TinyCategory { id: "play".into(), name: "Play".into(), position: 0 }];
        community.rooms = vec![
            TinyRoom { category_id: Some("talk".into()), position: 1, ..seated("lounge", &["mem", "me"]) },
            TinyRoom { category_id: Some("talk".into()), position: 0, ..seated("quiet", &[]) },
            TinyRoom { category_id: Some("play".into()), ..seated("games", &["mod"]) },
            seated("loose", &[]),
        ];
        community.online = vec!["own".into(), "me".into(), "mod".into(), "mem".into()];
        community
    }

    #[test]
    fn dragging_a_room_or_a_category_makes_the_whole_new_order() {
        let mut community = modern();
        let groups = layout_of(&community);
        let shape = |groups: &[TinyLayoutGroup]| groups.iter().map(|group| format!("{}:{}", group.category_id.as_deref().unwrap_or("-"), group.room_ids.join(","))).collect::<Vec<_>>();
        assert_eq!(shape(&groups), ["-:loose", "play:games", "talk:quiet,lounge"]);
        let moved = move_room(groups.clone(), "lounge", Some("play"), Some("games"));
        assert_eq!(shape(&moved), ["-:loose", "play:lounge,games", "talk:quiet"]);
        let moved = move_room(groups.clone(), "loose", Some("talk"), None);
        assert_eq!(shape(&moved), ["-:", "play:games", "talk:quiet,lounge,loose"]);
        let moved = move_category(groups, "talk", "play");
        assert_eq!(shape(&moved), ["-:loose", "talk:quiet,lounge", "play:games"]);
        apply_layout(&mut community, &moved);
        assert_eq!(shape(&layout_of(&community)), ["-:loose", "talk:quiet,lounge", "play:games"]);
    }

    #[test]
    fn the_channel_list_puts_loose_rooms_first_then_categories_in_order_and_seats_under_rooms() {
        let community = modern();
        let lines = lines(&community, &HashSet::new(), Some("me"));
        let shape: Vec<String> = lines
            .iter()
            .map(|line| match line {
                Line::Category { name, .. } => format!("# {name}"),
                Line::Room { id } => id.clone(),
                Line::Seat { user, .. } => format!("  {user}"),
            })
            .collect();
        assert_eq!(shape, ["loose", "# Play", "games", "  mod", "# Talk", "quiet", "lounge", "  mem", "  me"]);
        // Folded, a category keeps only the room we sit in.
        let folded = HashSet::from(["talk".to_owned(), "play".to_owned()]);
        let lines = super::lines(&community, &folded, Some("me"));
        assert!(lines.iter().any(|line| *line == Line::Room { id: "lounge".into() }));
        assert!(!lines.iter().any(|line| *line == Line::Room { id: "games".into() }));
        assert!(!lines.iter().any(|line| *line == Line::Room { id: "quiet".into() }));
    }

    #[test]
    fn an_older_server_has_one_header_over_every_room() {
        let community = legacy("member");
        let lines = lines(&community, &HashSet::new(), None);
        assert!(matches!(&lines[0], Line::Category { id: None, name, .. } if name == "Voice channels"));
        assert_eq!(lines.iter().filter(|line| matches!(line, Line::Room { .. })).count(), 2);
    }

    #[test]
    fn members_group_under_hoisted_roles_then_online_then_offline() {
        let mut community = modern();
        let groups = groups(&community, true, "");
        let titles: Vec<&str> = groups.iter().map(|group| group.title.as_str()).collect();
        assert_eq!(titles, ["Owner", "Admin", "Moderator", "Online", "Offline"]);
        assert_eq!(groups[2].people, ["mod"]);
        assert_eq!(groups[4].people, ["peer"]);
        assert!(groups[4].offline);
        // Without presence, nobody is offline.
        let titles: Vec<String> = super::groups(&community, false, "").into_iter().map(|group| group.title).collect();
        assert_eq!(titles, ["Owner", "Admin", "Moderator", "Members"]);
        // Searching keeps only who matches.
        community.members[4].email = "mo@example.com".into();
        let found = super::groups(&community, true, "EXAMPLE");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].people, ["mem"]);
    }

    #[test]
    fn the_person_menu_offers_only_what_rank_and_permission_allow() {
        let community = modern();
        let mo = person_menu(&community, "mem");
        assert!(mo.local);
        assert_eq!(mo.server_mute, Some(false));
        assert_eq!(mo.server_deafen, Some(false));
        assert_eq!(mo.move_to.len(), 3);
        assert!(mo.disconnect && mo.timeout && mo.kick && mo.ban);
        // Roles we may hand out are below our rank: not Admin.
        let names: Vec<&str> = mo.roles.iter().map(|role| role.name.as_str()).collect();
        assert_eq!(names, ["Moderator", "Crew"]);
        // Someone of our own rank, and the owner, are out of reach: no moderation at all.
        for peer in ["peer", "own"] {
            let menu = person_menu(&community, peer);
            assert!(!menu.moderates() && !menu.manages(), "{peer}");
            assert!(menu.local);
        }
        // Ourselves: no volume, no moderation.
        let me = person_menu(&community, "me");
        assert!(me.me && !me.local && !me.moderates() && !me.manages());
        // Without voice moderation on the server, mute, deafen and move are gone; disconnect stays.
        let mut older = community.clone();
        older.capabilities.features.retain(|feature| feature != "voice_moderation");
        let mo = person_menu(&older, "mem");
        assert!(mo.server_mute.is_none() && mo.server_deafen.is_none() && mo.move_to.is_empty());
        assert!(mo.disconnect);
        // A member with the default bits moderates nobody.
        let mut member = community.clone();
        member.access = Some(TinyAccess { owner: false, roles: vec![], permissions: TINY_EVERYONE_DEFAULT, rank: 0 });
        let mo = person_menu(&member, "mem");
        assert!(!mo.moderates() && !mo.manages());
    }

    #[test]
    fn an_older_server_offers_its_three_roles_kick_and_disconnect_by_the_saved_role() {
        let owner = legacy("owner");
        let mo = person_menu(&owner, "mem");
        assert!(mo.disconnect && mo.kick);
        assert!(!mo.ban && !mo.timeout && mo.server_mute.is_none());
        assert_eq!(mo.legacy_roles, [("owner", false), ("admin", false), ("member", true)]);
        let admin = legacy("admin");
        assert_eq!(person_menu(&admin, "mem").legacy_roles, [("admin", false), ("member", true)]);
        assert!(!person_menu(&admin, "adm").kick, "an admin does not remove another admin");
        assert!(!person_menu(&admin, "own").manages());
        let member = legacy("member");
        assert!(!person_menu(&member, "mem").manages());
        assert!(!person_menu(&member, "mem").disconnect);
    }

    #[test]
    fn what_may_be_done_follows_permissions_and_the_servers_features() {
        let modern = may(&modern());
        assert!(modern.invite && modern.invite_options && modern.invite_roles && modern.layout && modern.edit_server && modern.read_bans);
        assert!(modern.invite_forever && !modern.owner);
        let old = may(&legacy("owner"));
        assert!(old.invite && !old.invite_options && !old.layout && !old.edit_server && !old.read_bans && old.owner);
        let member = may(&legacy("member"));
        assert!(!member.invite && !member.make_room && !member.see_email);
    }

    #[test]
    fn rooms_seats_and_words() {
        let mut room = seated("r", &["a", "b", "c"]);
        assert_eq!(occupancy(&room), "3");
        room.user_limit = 3;
        assert_eq!(occupancy(&room), "3 / 3");
        assert!(full(&room));
        let seat = TinySeat { muted: true, server_muted: true, priority: true, ..TinySeat::default() };
        assert_eq!(seat_states(&seat), ["Server muted", "Priority speaker"]);
        let seat = TinySeat { muted: true, deafened: true, ..TinySeat::default() };
        assert_eq!(seat_states(&seat), ["Deafened"]);
        assert_eq!(hex("#3FBF7A"), Some(0x3fbf7a));
        assert_eq!(hex("green"), None);
        assert_eq!(invite_terms(Some(1), Some(604_800)), "It works once, for 7 days, and will not be shown again.");
        assert_eq!(invite_terms(None, None), "It works any number of times, never expires, and will not be shown again.");
        assert_eq!(relative(1_000, 1_000 - 300), "5 min ago");
        assert_eq!(relative(0, 3 * 86_400), "in 3 days");
        assert_eq!(server_name(&legacy("member"), ""), "tiny.example");
    }
}
