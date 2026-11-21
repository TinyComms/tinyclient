//! People in a tenant: who wrote a message, who a chat is with, who is online. The directory is
//! searched and looked up by id, never listed.

use super::{escaped, iso_utc, Auth, Body, Fault, Teams, GRAPH, PRESENCE, REMEMBERED, SPACES};
use crate::adapters::{About, Members};
use crate::store::{PersonDetails, PersonRow};
use protocol::{Activity, ActivityKind, Contact, ContactKind, Mark, MediaFile, MediaKind, MemberSection, Person, Presence, Relation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Ids per lookup. The official client asks for far more at once; nothing here needs to.
const BATCH: usize = 100;
/// Pictures are registered at list size, and at twice that for a dense screen.
const AVATAR: u32 = 64;
/// People whose presence is asked for in one call, and how soon the same people are asked
/// about again.
const WATCHED: usize = 200;
const AGAIN: Duration = Duration::from_secs(60);

/// What tinyclient keeps of a person. Stored as a note, so a restart asks Teams nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct Known {
    #[serde(rename = "n")]
    pub name: String,
    /// Their sign-in name, which Teams' links name people by.
    #[serde(rename = "m", default)]
    pub mail: String,
    #[serde(rename = "t", default)]
    pub title: String,
    #[serde(rename = "d", default)]
    pub department: String,
    #[serde(rename = "b", default)]
    pub bot: bool,
    /// A guest in this tenant.
    #[serde(rename = "g", default)]
    pub guest: bool,
}

/// Extracts the Entra object id from a Teams user MRI (`8:orgid:<oid>`).
pub(super) fn oid_of(mri: &str) -> Option<&str> {
    let oid = mri.strip_prefix("8:orgid:")?;
    (!oid.is_empty() && !oid.contains(['/', '?', '#', ' '])).then_some(oid)
}

/// Maps common Microsoft Windows and IANA timezone names (plus `UTC±H[:MM]`) to standard UTC
/// offsets in seconds east of UTC, so a colleague's local time can be drawn on their card.
pub(super) fn timezone_offset_secs(tz: &str) -> Option<i32> {
    let trimmed = tz.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("UTC").or_else(|| trimmed.strip_prefix("GMT")) {
        if rest.is_empty() {
            return Some(0);
        }
        let sign = match rest.as_bytes().first() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => 0,
        };
        if sign != 0 {
            let digits = &rest[1..];
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit() || b == b':') {
                let (h, m) = match digits.split_once(':') {
                    Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
                    None => (digits.parse::<u32>().ok()?, 0),
                };
                if (h < 14 && m < 60) || (h == 14 && m == 0) {
                    return Some(sign * (h as i32 * 3600 + m as i32 * 60));
                }
            }
        }
    }
    let hours = match trimmed {
        "UTC" | "Etc/UTC" | "GMT Standard Time" | "Greenwich Standard Time" | "Europe/London" | "Europe/Lisbon" | "Europe/Dublin" | "Atlantic/Reykjavik" => 0.0,
        "W. Europe Standard Time" | "Central Europe Standard Time" | "Central European Standard Time" | "Romance Standard Time"
        | "Europe/Berlin" | "Europe/Paris" | "Europe/Madrid" | "Europe/Rome" | "Europe/Amsterdam" | "Europe/Zurich" | "Europe/Stockholm" | "Europe/Warsaw" => 1.0,
        "E. Europe Standard Time" | "FLE Standard Time" | "GTB Standard Time" | "Israel Standard Time" | "South Africa Standard Time"
        | "Europe/Helsinki" | "Europe/Athens" | "Europe/Bucharest" | "Asia/Jerusalem" | "Africa/Johannesburg" => 2.0,
        "Turkey Standard Time" | "Arab Standard Time" | "Russian Standard Time" | "Europe/Istanbul" | "Europe/Moscow" | "Asia/Riyadh" => 3.0,
        "Arabian Standard Time" | "Asia/Dubai" => 4.0,
        "Pakistan Standard Time" | "Asia/Karachi" => 5.0,
        "India Standard Time" | "Asia/Kolkata" | "Asia/Calcutta" => 5.5,
        "SE Asia Standard Time" | "Asia/Bangkok" | "Asia/Jakarta" => 7.0,
        "China Standard Time" | "Singapore Standard Time" | "Taipei Standard Time" | "W. Australia Standard Time"
        | "Asia/Shanghai" | "Asia/Hong_Kong" | "Asia/Singapore" | "Asia/Taipei" | "Australia/Perth" => 8.0,
        "Tokyo Standard Time" | "Korea Standard Time" | "Asia/Tokyo" | "Asia/Seoul" => 9.0,
        "AUS Eastern Standard Time" | "E. Australia Standard Time" | "Australia/Sydney" | "Australia/Melbourne" | "Australia/Brisbane" => 10.0,
        "New Zealand Standard Time" | "Pacific/Auckland" => 12.0,
        "Hawaiian Standard Time" | "Pacific/Honolulu" => -10.0,
        "Alaskan Standard Time" | "America/Anchorage" => -9.0,
        "Pacific Standard Time" | "America/Los_Angeles" | "America/Vancouver" => -8.0,
        "Mountain Standard Time" | "US Mountain Standard Time" | "America/Denver" | "America/Phoenix" => -7.0,
        "Central Standard Time" | "Central Standard Time (Mexico)" | "America/Chicago" | "America/Mexico_City" => -6.0,
        "Eastern Standard Time" | "US Eastern Standard Time" | "America/New_York" | "America/Toronto" => -5.0,
        "Atlantic Standard Time" | "America/Halifax" => -4.0,
        "E. South America Standard Time" | "Argentina Standard Time" | "America/Sao_Paulo" | "America/Argentina/Buenos_Aires" => -3.0,
        _ => return None,
    };
    Some((hours * 3600.0) as i32)
}

/// Extracts an active out-of-office `(message, until_unix)` from a Teams unified presence
/// payload. Checks both `outOfOfficeNote` (legacy / web presence) and `outOfOfficeSettings`
/// (Graph / unified presence), as well as `calendarData.isOutOfOffice` and `activity == "OutOfOffice"`.
pub(super) fn parse_out_of_office(presence: &Value) -> Option<(String, Option<i64>)> {
    let doing_now = presence["activity"].as_str().unwrap_or("");
    let note_obj = &presence["outOfOfficeNote"];
    let settings_obj = &presence["outOfOfficeSettings"];
    let settings_active = settings_obj["isOutOfOffice"] == true;
    let note_msg = note_obj["message"]
        .as_str()
        .map(|raw| super::html::parse(raw, &Nobody, "").text.trim().to_owned())
        .filter(|s| !s.is_empty());
    let settings_msg = if settings_active {
        [settings_obj["message"].as_str(), settings_obj["outOfOfficeMessage"].as_str()]
            .into_iter()
            .flatten()
            .map(|raw| super::html::parse(raw, &Nobody, "").text.trim().to_owned())
            .find(|s| !s.is_empty())
    } else {
        None
    };
    let raw_msg = note_msg.or(settings_msg);
    let is_ooo = settings_active
        || presence["calendarData"]["isOutOfOffice"] == true
        || doing_now == "OutOfOffice"
        || raw_msg.is_some();
    if !is_ooo {
        return None;
    }
    let until = [
        note_obj["expiry"].as_str(),
        settings_active.then(|| settings_obj["expirationDateTime"].as_str()).flatten(),
        settings_active
            .then(|| settings_obj["scheduledEndDateTime"]["dateTime"].as_str())
            .flatten(),
        settings_active.then(|| settings_obj["expiry"].as_str()).flatten(),
    ]
    .into_iter()
    .flatten()
    .map(super::seconds)
    .find(|&ts| ts > 0);
    Some((raw_msg.unwrap_or_else(|| "Out of office".to_owned()), until))
}

/// Parses Microsoft Graph `automaticRepliesSetting` JSON into `protocol::OutOfOffice`.
pub(super) fn parse_automatic_replies(setting: &Value) -> protocol::OutOfOffice {
    let status = setting["status"].as_str().unwrap_or("disabled");
    let enabled = !status.eq_ignore_ascii_case("disabled") && !status.is_empty();
    let strip = |raw: Option<&str>| {
        raw.map(|s| super::html::parse(s, &Nobody, "").text.trim().to_owned())
            .unwrap_or_default()
    };
    let start = setting["scheduledStartDateTime"]["dateTime"]
        .as_str()
        .map(super::seconds)
        .filter(|&ts| ts > 0 && status.eq_ignore_ascii_case("scheduled"));
    let end = setting["scheduledEndDateTime"]["dateTime"]
        .as_str()
        .map(super::seconds)
        .filter(|&ts| ts > 0 && status.eq_ignore_ascii_case("scheduled"));
    let audience = setting["externalAudience"].as_str().unwrap_or("none");
    let send_external = !audience.eq_ignore_ascii_case("none") && !audience.is_empty();
    protocol::OutOfOffice {
        enabled,
        message: strip(setting["internalReplyMessage"].as_str()),
        start,
        end,
        send_external,
        external_message: strip(setting["externalReplyMessage"].as_str()),
    }
}

/// Most people whose out-of-office note is held, from the presence answers.
const AWAY_KEPT: usize = 500;

/// Teams' availability as the one presence model: Busy (in a meeting, in a call) is its own
/// state, not do-not-disturb.
fn presence_of(availability: &str) -> Presence {
    match availability {
        "Available" => Presence::Online,
        "AvailableIdle" | "Away" | "BeRightBack" => Presence::Idle,
        "Busy" | "BusyIdle" => Presence::Busy,
        "DoNotDisturb" | "Focusing" | "Presenting" => Presence::Dnd,
        "Offline" => Presence::Offline,
        _ => Presence::Unknown,
    }
}

/// What someone is doing, when it is more than their availability already says.
fn activity(activity: &str) -> &'static str {
    match activity {
        "InACall" => "In a call",
        "InAConferenceCall" => "In a call",
        "InAMeeting" => "In a meeting",
        "Presenting" => "Presenting",
        "OutOfOffice" => "Out of office",
        "Focusing" => "Focusing",
        _ => "",
    }
}

/// What someone is doing, as the shared activity kinds.
fn doing(activity: &str) -> Option<ActivityKind> {
    Some(match activity {
        "InAMeeting" | "InAConferenceCall" => ActivityKind::InMeeting,
        "InACall" => ActivityKind::InCall,
        "Presenting" => ActivityKind::Presenting,
        _ => return None,
    })
}

/// A team thread, whose roster has owners, members and guests. A chat does not.
fn team_thread(described: &Value, thread: &str) -> bool {
    described["properties"]["threadType"] == "space" || thread.contains("@thread.tacv2")
}

/// Where a thread role sits. `Admin` is an owner. `Guest` is a guest. The rest are members.
pub(super) fn section_of(role: &str) -> &'static str {
    match role {
        "Admin" => "Owners",
        "Guest" => "Guests",
        _ => "Members",
    }
}

fn section_rank(role: &str) -> u8 {
    match section_of(role) {
        "Owners" => 0,
        "Guests" => 2,
        _ => 1,
    }
}

/// The word a team's info pane uses for the same role.
pub(super) fn role_label(role: &str) -> Option<&'static str> {
    Some(match role {
        "Admin" => "Owner",
        "User" => "Member",
        "Guest" => "Guest",
        _ => return None,
    })
}

fn team_sections(roster: &[(&str, &str, &str)]) -> Vec<MemberSection> {
    let mut owners = 0u32;
    let mut members = 0u32;
    let mut guests = 0u32;
    for (_, _, role) in roster {
        match section_of(role) {
            "Owners" => owners += 1,
            "Guests" => guests += 1,
            _ => members += 1,
        }
    }
    let mut sections = Vec::new();
    if owners > 0 {
        sections.push(MemberSection { name: "Owners".to_owned(), count: owners });
    }
    if members > 0 {
        sections.push(MemberSection { name: "Members".to_owned(), count: members });
    }
    if guests > 0 {
        sections.push(MemberSection { name: "Guests".to_owned(), count: guests });
    }
    sections
}

/// The lab's teams-team-info check: a team roster is owners, members, then guests.
pub(super) fn prove_team_roster() -> Result<(), String> {
    let roles = ["Admin", "User", "Guest", "Guest"];
    let roster: Vec<(&str, &str, &str)> = roles.iter().map(|role| ("", "", *role)).collect();
    let counts: Vec<(String, u32)> = team_sections(&roster).into_iter().map(|section| (section.name, section.count)).collect();
    let expected = [("Owners", 1u32), ("Members", 1), ("Guests", 2)];
    let same = counts.len() == expected.len() && counts.iter().zip(expected).all(|((name, count), (want, n))| name == want && *count == n);
    if !same {
        return Err(format!("a team roster was sectioned as {counts:?}"));
    }
    if role_label("Admin") != Some("Owner") || role_label("User") != Some("Member") || role_label("Guest") != Some("Guest") {
        return Err("a team role is not named".to_owned());
    }
    Ok(())
}

impl Teams {
    /// Someone already looked up, from memory or the store. Never asks Teams.
    pub(super) fn cached(&self, id: &str) -> Option<Known> {
        if let Some(known) = self.state().people.get(id) {
            return Some(known.clone());
        }
        let known: Known = serde_json::from_str(&self.ctx.note(&format!("u:{id}"))?).ok()?;
        let mut state = self.state();
        if state.people.len() >= REMEMBERED {
            state.people.clear();
        }
        state.people.insert(id.to_owned(), known.clone());
        Some(known)
    }

    pub(super) fn name_of(&self, id: &str) -> Option<String> {
        self.cached(id).map(|known| known.name).filter(|name| !name.is_empty())
    }

    /// A person's picture, registered for fetching the first time it is wanted this run. Teams
    /// serves one for everyone in a tenant, drawn from their initials when they set none.
    /// CONTRACT: the address does not change when the picture does, so a new picture is not a
    /// new id; it shows once the cached file is evicted.
    pub(super) fn picture(&self, id: &str, name: &str) -> Option<MediaFile> {
        if !id.starts_with("8:") {
            return None;
        }
        let place = self.state().place.clone()?;
        let file = MediaFile {
            width: AVATAR,
            height: AVATAR,
            ..MediaFile::new(format!("avatar:{id}"), MediaKind::Avatar)
        };
        let first = {
            let mut state = self.state();
            if state.noted.len() >= 4 * REMEMBERED {
                state.noted.clear();
            }
            state.noted.insert(file.id.clone())
        };
        if first {
            let at = |size: u32| {
                format!("{}/beta/users/{}/profilepicturev2?displayname={}&size=HR{size}x{size}", place.middle, escaped(id), escaped(name))
            };
            let registered = self.ctx.media(&[(file.clone(), at(AVATAR))]).and_then(|()| self.ctx.media_variants(&file.id, &[(AVATAR * 2, at(AVATAR * 2))]));
            if let Err(err) = registered {
                tracing::warn!("chatd: Teams: {err}");
            }
        }
        Some(file)
    }

    fn link(mail: &str) -> String {
        if mail.is_empty() {
            return String::new();
        }
        format!("msteams:/l/chat/0/0?users={}", escaped(mail))
    }

    /// Reads Teams' profile objects, registers their pictures and records them as people.
    fn learn(&self, users: &[Value]) -> Vec<(String, Known)> {
        let mut learned = Vec::new();
        for user in users {
            let Some(id) = user["mri"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            let text = |name: &str| user[name].as_str().unwrap_or("").trim().to_owned();
            let name = match text("displayName") {
                name if name.is_empty() => format!("{} {}", text("givenName"), text("surname")).trim().to_owned(),
                name => name,
            };
            let known = Known {
                name,
                mail: Some(text("userPrincipalName")).filter(|mail| !mail.is_empty()).unwrap_or_else(|| text("email")),
                title: text("jobTitle"),
                department: text("department"),
                bot: id.starts_with("28:") || user["type"].as_str().is_some_and(|kind| kind.eq_ignore_ascii_case("bot")),
                guest: user["userType"].as_str().is_some_and(|kind| kind.eq_ignore_ascii_case("guest")),
            };
            learned.push((id.to_owned(), known));
        }
        let rows: Vec<PersonRow> = learned.iter().map(|(id, known)| self.row(id, known)).collect();
        if let Err(err) = self.ctx.people(&rows) {
            tracing::warn!("chatd: Teams: {err}");
        }
        let details: Vec<(String, PersonDetails)> = learned
            .iter()
            .map(|(id, known)| {
                let mark = known.guest.then_some(Mark::Guest);
                (id.clone(), PersonDetails { title: known.title.clone(), mark, ..PersonDetails::default() })
            })
            .collect();
        if let Err(err) = self.ctx.details(&details) {
            tracing::warn!("chatd: Teams: {err}");
        }
        let mut state = self.state();
        if state.people.len() + learned.len() > REMEMBERED {
            state.people.clear();
        }
        for (id, known) in &learned {
            self.ctx.set_note(&format!("u:{id}"), &serde_json::to_string(known).unwrap_or_default());
            state.people.insert(id.clone(), known.clone());
        }
        learned
    }

    fn row(&self, id: &str, known: &Known) -> PersonRow {
        PersonRow {
            id: id.to_owned(),
            name: if known.name.is_empty() { known.mail.clone() } else { known.name.clone() },
            handle: known.mail.clone(),
            avatar: self.picture(id, &known.name).map(|file| file.id),
            bot: known.bot,
            status: String::new(),
            link: Teams::link(&known.mail),
        }
    }

    fn middle(&self, path: &str, body: Body) -> Result<Value, Fault> {
        let url = format!("{}{path}", self.place()?.middle);
        Ok(self.call("POST", &url, Auth::Both(SPACES), body, true)?.body)
    }

    /// The people an answer of the middle tier lists, under whichever name it used.
    fn listed(answer: &Value) -> &[Value] {
        let lists = [answer, &answer["value"], &answer["resolvedUsers"]];
        lists.into_iter().find_map(Value::as_array).map_or(&[][..], Vec::as_slice)
    }

    /// Looks up everyone in `ids` not yet known, together. Someone Teams will not name is left
    /// unknown: a message shows the name it came with.
    pub(super) fn lookup(&self, ids: &[String]) {
        // People (`8:`) and apps and bots (`28:`), whose names the same call gives with
        // `includeBots`.
        let mut unknown: Vec<&String> = ids.iter().filter(|id| (id.starts_with("8:") || id.starts_with("28:")) && self.cached(id).is_none()).collect();
        unknown.sort();
        unknown.dedup();
        for batch in unknown.chunks(BATCH) {
            let path = "/beta/users/fetchShortProfile?isMailAddress=false&canBeSmtpAddress=false&enableGuest=true\
                        &includeIBBarredUsers=true&skypeTeamsInfo=true&includeBots=true";
            match self.middle(path, Body::Json(&json!(batch))) {
                Ok(found) => drop(self.learn(Teams::listed(&found))),
                Err(Fault::Auth(why)) => {
                    self.state().expired.get_or_insert(why);
                    return;
                }
                Err(_) => return,
            }
        }
    }

    /// The tenant's directory, searched on Teams' side. At most 25 land in the store.
    pub(super) fn search(&self, query: &str) -> Result<(), Fault> {
        let path = "/beta/users/searchV2?includeDLs=false&includeBots=false&enableGuest=true&source=newChat&skypeTeamsInfo=true";
        let found = self.middle(path, Body::Text(query))?;
        let people = Teams::listed(&found);
        self.learn(&people[..people.len().min(25)]);
        Ok(())
    }

    fn person(&self, id: &str, known: &Known) -> Person {
        Person {
            key: self.ctx.key(id),
            name: if known.name.is_empty() { id.to_owned() } else { known.name.clone() },
            handle: known.mail.clone(),
            avatar: self.picture(id, &known.name),
            bot: known.bot,
            relation: if self.state().partners.contains_key(id) { Relation::Contact } else { Relation::None },
            presence: self.ctx.presence_of(id),
            status: String::new(),
            link: Teams::link(&known.mail),
            ..Person::new(self.ctx.key(id), "")
        }
    }

    /// One window of a conversation's members. A team is owners, then members, then guests.
    /// A chat stays one list: there the role is `Admin` for everyone.
    pub(super) fn members(&self, conversation: &str, offset: u32, limit: u32) -> Result<Members, Fault> {
        let thread = super::thread_of(conversation);
        let described = self.chat("GET", &format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread)), Body::None, true)?.body;
        let all: Vec<(&str, &str, &str)> = described["members"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|member| Some((member["id"].as_str()?, member["friendlyName"].as_str().unwrap_or(""), member["role"].as_str().unwrap_or(""))))
            .collect();
        let team = team_thread(&described, thread);
        let mut ordered = all.clone();
        if team {
            ordered.sort_by_key(|(_, _, role)| section_rank(role));
        }
        let window: Vec<(&str, &str)> = ordered.iter().skip(offset as usize).take(limit as usize).map(|(id, friendly, _)| (*id, *friendly)).collect();
        let ids: Vec<String> = window.iter().map(|(id, _)| (*id).to_owned()).collect();
        self.lookup(&ids);
        let members = window
            .iter()
            .map(|(id, friendly)| {
                let known = self.cached(id).unwrap_or_else(|| Known {
                    name: (*friendly).to_owned(),
                    bot: id.starts_with("28:"),
                    ..Known::default()
                });
                self.person(id, &known)
            })
            .collect();
        Ok(Members {
            pending: false,
            sections: if team { team_sections(&all) } else { vec![MemberSection { name: "Members".to_owned(), count: all.len() as u32 }] },
            members,
        })
    }

    /// The chat with a person, created on Teams if there was none. Creating it sends them
    /// nothing: an empty chat is not shown to its other side.
    pub(super) fn start_direct(&self, person: &str) -> Result<String, Fault> {
        if let Some(chat) = self.state().partners.get(person) {
            return Ok(chat.clone());
        }
        let me = self.me();
        let wanted = json!({
            "members": [{"id": me, "role": "Admin"}, {"id": person, "role": "Admin"}],
            "properties": {"threadType": "chat", "fixedRoster": true, "uniquerosterthread": true},
        });
        let created = self.chat("POST", "/v1/threads", Body::Json(&wanted), false)?;
        let named = created.location.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("");
        let id = match named {
            "" => super::id_of(&created.body["id"]),
            named => named.replace("%3A", ":").replace("%3a", ":").replace("%40", "@"),
        };
        if !id.starts_with("19:") {
            return Err(Fault::Net("Teams created the chat but did not name it".to_owned()));
        }
        self.lookup(&[person.to_owned()]);
        if !self.known(&id)? {
            return Err(Fault::Net("Teams does not describe the chat it created".to_owned()));
        }
        // The description may not have arrived yet; the row is ours either way.
        self.state().partners.insert(person.to_owned(), id.clone());
        self.ctx.relation(person, Relation::Contact)?;
        Ok(id)
    }

    fn away_of(&self, person: &str) -> (String, Option<i64>) {
        if let Some(cached) = self.state().away.get(person).cloned() {
            return cached;
        }
        let Some(raw) = self.ctx.note(&format!("ooo:{person}")).filter(|s| !s.is_empty()) else {
            return (String::new(), None);
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
            return (String::new(), None);
        };
        let msg = parsed["m"].as_str().unwrap_or("").to_owned();
        let until = parsed["u"].as_i64().filter(|&ts| ts > 0);
        if !msg.is_empty() {
            self.state().away.insert(person.to_owned(), (msg.clone(), until));
        }
        (msg, until)
    }

    fn save_away(&self, person: &str, away: Option<(String, Option<i64>)>) {
        let mut state = self.state();
        if let Some((msg, until)) = away {
            if state.away.len() >= AWAY_KEPT {
                state.away.clear();
            }
            state.away.insert(person.to_owned(), (msg.clone(), until));
            drop(state);
            let persisted = json!({"m": msg, "u": until}).to_string();
            self.ctx.set_note(&format!("ooo:{person}"), &persisted);
        } else {
            state.away.remove(person);
            drop(state);
            self.ctx.set_note(&format!("ooo:{person}"), "");
        }
    }

    /// Best-effort fetch of presence (including out-of-office note and return date) for a set of MRIs.
    pub(super) fn fetch_presence_for(&self, ids: &[String]) {
        let mris: Vec<&String> = ids.iter().filter(|id| id.starts_with("8:")).collect();
        if mris.is_empty() {
            return;
        }
        let Ok(place) = self.place() else {
            return;
        };
        let asked: Vec<Value> = mris.iter().map(|id| json!({"mri": id})).collect();
        let url = format!("{}/v1/presence/getpresence/", place.presence);
        if let Ok(found) = self.call("POST", &url, Auth::Bearer(PRESENCE), Body::Json(&json!(asked)), true) {
            self.presences(&found.body);
        }
    }

    /// Forces a fresh profile picture fetch the next time the avatar is requested (Teams keeps
    /// the same URL even after someone changes their photo).
    fn refresh_picture(&self, person: &str, name: &str) {
        if !person.starts_with("8:") {
            return;
        }
        let id = format!("avatar:{person}");
        self.ctx.media_evict(&id);
        self.state().noted.remove(&id);
        let _ = self.picture(person, name);
    }

    /// Learns a Microsoft Graph user object (`manager` or `directReports` entry) into the local
    /// store so clicking them in the org chart opens their card cleanly.
    fn learn_graph_person(&self, user: &Value) -> Option<protocol::ProfilePerson> {
        if let Some(odata) = user["@odata.type"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            let name = odata.rsplit(['.', '#']).next().unwrap_or(odata);
            if !name.eq_ignore_ascii_case("user") {
                return None;
            }
        }
        let oid = user["id"].as_str().map(str::trim).filter(|s| {
            !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '?' | '#'))
        })?;
        let mri = format!("8:orgid:{oid}");
        let name = user["displayName"].as_str().unwrap_or("").trim().to_owned();
        if name.is_empty() {
            return None;
        }
        let title = user["jobTitle"].as_str().unwrap_or("").trim().to_owned();
        let mail = user["mail"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| user["userPrincipalName"].as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        let department = user["department"].as_str().unwrap_or("").trim().to_owned();
        let known = Known {
            name: name.clone(),
            mail,
            title: title.clone(),
            department,
            bot: false,
            guest: user["userType"].as_str().is_some_and(|k| k.eq_ignore_ascii_case("guest")),
        };
        let row = self.row(&mri, &known);
        let _ = self.ctx.people(&[row]);
        let _ = self.ctx.details(&[(
            mri.clone(),
            PersonDetails { title: known.title.clone(), mark: known.guest.then_some(Mark::Guest), ..PersonDetails::default() },
        )]);
        self.ctx.set_note(&format!("u:{mri}"), &serde_json::to_string(&known).unwrap_or_default());
        self.state().people.insert(mri.clone(), known);
        Some(protocol::ProfilePerson {
            key: self.ctx.key(&mri),
            name,
            title,
        })
    }

    /// One person's card: what their organization says of them, enriched from Microsoft Graph
    /// (phones, office, pronouns, local time, manager, direct reports) with graceful fallback
    /// for guests and tenants where Graph user/org calls are restricted.
    pub(super) fn profile(&self, person: &str) -> Result<About, Fault> {
        let path = "/beta/users/fetchShortProfile?isMailAddress=false&canBeSmtpAddress=false&enableGuest=true\
                    &includeIBBarredUsers=true&skypeTeamsInfo=true&includeBots=true";
        let found = self.middle(path, Body::Json(&json!([person])))?;
        let short_entries = Teams::listed(&found);
        let raw_short = short_entries.iter().find(|u| u["mri"].as_str() == Some(person));
        let learned = self.learn(short_entries);
        let known = learned.into_iter().find(|(id, _)| id == person).map(|(_, known)| known).or_else(|| self.cached(person));
        let mut known = known.unwrap_or_default();
        self.refresh_picture(person, &known.name);
        self.fetch_presence_for(&[person.to_owned()]);
        let (away, away_until) = self.away_of(person);

        let mut office = raw_short
            .and_then(|u| u["userLocation"].as_str().or_else(|| u["officeLocation"].as_str()))
            .unwrap_or("")
            .trim()
            .to_owned();
        let mut pronouns = raw_short
            .and_then(|u| u["pronouns"].as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        let mut time_zone = String::new();
        let mut phones: Vec<String> = Vec::new();
        let mut push_phone = |raw: &str| {
            let trimmed = raw.trim();
            if !trimmed.is_empty() && !phones.iter().any(|p| p == trimmed) {
                phones.push(trimmed.to_owned());
            }
        };
        if let Some(u) = raw_short {
            for field in ["telephoneNumber", "mobile", "phoneNumber"] {
                if let Some(val) = u[field].as_str() {
                    push_phone(val);
                }
            }
        }

        let mut manager = None;
        let mut reports = Vec::new();
        if let Some(oid) = oid_of(person) {
            let user_url = format!(
                "{}/v1.0/users/{}?$select=displayName,mail,userPrincipalName,jobTitle,department,officeLocation,businessPhones,mobilePhone,mailboxSettings,userType",
                self.graph,
                escaped(oid)
            );
            if let Ok(answer) = self.call("GET", &user_url, Auth::Bearer(GRAPH), Body::None, true) {
                let u = &answer.body;
                if let Some(v) = u["jobTitle"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    if known.title.is_empty() {
                        known.title = v.to_owned();
                    }
                }
                if let Some(v) = u["department"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    if known.department.is_empty() {
                        known.department = v.to_owned();
                    }
                }
                if let Some(v) = u["mail"].as_str().or_else(|| u["userPrincipalName"].as_str()).map(str::trim).filter(|s| !s.is_empty()) {
                    if known.mail.is_empty() {
                        known.mail = v.to_owned();
                    }
                }
                if let Some(v) = u["officeLocation"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    office = v.to_owned();
                }
                if let Some(v) = u["pronouns"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    pronouns = v.to_owned();
                }
                if let Some(arr) = u["businessPhones"].as_array() {
                    for p in arr.iter().filter_map(Value::as_str) {
                        push_phone(p);
                    }
                }
                if let Some(p) = u["mobilePhone"].as_str() {
                    push_phone(p);
                }
                if let Some(tz) = u["mailboxSettings"]["timeZone"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    time_zone = tz.to_owned();
                }
            }
            let mgr_url = format!(
                "{}/v1.0/users/{}/manager?$select=id,displayName,jobTitle,mail,userPrincipalName,department",
                self.graph,
                escaped(oid)
            );
            if let Ok(answer) = self.call("GET", &mgr_url, Auth::Bearer(GRAPH), Body::None, true) {
                manager = self.learn_graph_person(&answer.body);
            }
            let rep_url = format!(
                "{}/v1.0/users/{}/directReports?$top=10&$select=id,displayName,jobTitle,mail,userPrincipalName,department",
                self.graph,
                escaped(oid)
            );
            if let Ok(answer) = self.call("GET", &rep_url, Auth::Bearer(GRAPH), Body::None, true) {
                for entry in answer.body["value"].as_array().into_iter().flatten().take(10) {
                    if let Some(rp) = self.learn_graph_person(entry) {
                        reports.push(rp);
                    }
                }
            }
        }

        let utc_offset = timezone_offset_secs(&time_zone);
        if utc_offset.is_some() || !known.title.is_empty() {
            let mark = known.guest.then_some(Mark::Guest);
            let _ = self.ctx.details(&[(
                person.to_owned(),
                PersonDetails { title: known.title.clone(), mark, utc_offset },
            )]);
        }

        let mut contacts = Vec::new();
        if !known.mail.is_empty() {
            contacts.push(Contact { kind: ContactKind::Email, value: known.mail, masked: false });
        }
        for phone in phones {
            contacts.push(Contact { kind: ContactKind::Phone, value: phone, masked: true });
        }

        Ok(About {
            title: known.title,
            department: known.department,
            office,
            pronouns,
            time_zone,
            utc_offset,
            contacts,
            away,
            away_until,
            manager,
            reports,
            ..About::default()
        })
    }

    /// Reads our Exchange/Teams automatic replies setting from Microsoft Graph.
    pub(super) fn get_out_of_office(&self) -> Result<protocol::OutOfOffice, Fault> {
        let url = format!("{}/v1.0/me/mailboxSettings/automaticRepliesSetting", self.graph);
        let answer = self.call("GET", &url, Auth::Bearer(GRAPH), Body::None, true)?.body;
        let setting = if answer["automaticRepliesSetting"].is_object() {
            &answer["automaticRepliesSetting"]
        } else {
            &answer
        };
        Ok(parse_automatic_replies(setting))
    }

    /// Writes our Exchange/Teams automatic replies setting via Microsoft Graph `PATCH /v1.0/me/mailboxSettings`.
    pub(super) fn set_out_of_office(&self, ooo: &protocol::OutOfOffice) -> Result<protocol::OutOfOffice, Fault> {
        if ooo.enabled {
            match (ooo.start, ooo.end) {
                (Some(_), None) | (None, Some(_)) => {
                    return Err(Fault::Refused {
                        status: 400,
                        message: "Scheduled out-of-office requires both a start and an end time.".to_owned(),
                    });
                }
                (Some(start), Some(end)) if start <= 0 || end <= start => {
                    return Err(Fault::Refused {
                        status: 400,
                        message: "Out-of-office end time must be after start time.".to_owned(),
                    });
                }
                _ => {}
            }
        }
        let status = if !ooo.enabled {
            "disabled"
        } else if ooo.start.is_some() && ooo.end.is_some() {
            "scheduled"
        } else {
            "alwaysEnabled"
        };
        let internal_plain: String = ooo.message.trim().chars().take(2000).collect();
        let external_raw = if ooo.send_external {
            if ooo.external_message.trim().is_empty() {
                internal_plain.as_str()
            } else {
                ooo.external_message.trim()
            }
        } else {
            ""
        };
        let external_plain: String = external_raw.chars().take(2000).collect();
        let external_audience = if ooo.enabled && ooo.send_external { "all" } else { "none" };
        let mut setting = json!({
            "status": status,
            "internalReplyMessage": escape_ooo_html(&internal_plain),
            "externalAudience": external_audience,
            "externalReplyMessage": escape_ooo_html(&external_plain),
        });
        if let (Some(start), Some(end)) = (ooo.start, ooo.end) {
            if status == "scheduled" {
                setting["scheduledStartDateTime"] = json!({"dateTime": iso_utc(start), "timeZone": "UTC"});
                setting["scheduledEndDateTime"] = json!({"dateTime": iso_utc(end), "timeZone": "UTC"});
            }
        }
        let url = format!("{}/v1.0/me/mailboxSettings", self.graph);
        let body = json!({"automaticRepliesSetting": setting});
        let answer = self.call("PATCH", &url, Auth::Bearer(GRAPH), Body::Json(&body), false)?.body;
        let saved = if answer["automaticRepliesSetting"].is_object() {
            parse_automatic_replies(&answer["automaticRepliesSetting"])
        } else {
            let mut saved = ooo.clone();
            saved.message = internal_plain;
            if ooo.send_external {
                saved.external_message = external_plain;
            }
            saved
        };
        let me = self.me();
        if !me.is_empty() {
            if saved.enabled {
                let msg = if saved.message.is_empty() { "Out of office".to_owned() } else { saved.message.clone() };
                self.save_away(&me, Some((msg, saved.end)));
                self.ctx.presence(&me, self.ctx.presence_of(&me), "Out of office", true);
            } else {
                self.save_away(&me, None);
                self.ctx.presence(&me, self.ctx.presence_of(&me), "", true);
            }
        }
        Ok(saved)
    }

    // Presence.

    /// `{"mri", "presence": {"availability", "activity", "note": {"message"}}}`, from the call
    /// that asks for it or from push.
    pub(super) fn presences(&self, listed: &Value) {
        for entry in listed.as_array().into_iter().flatten() {
            let Some(id) = entry["mri"].as_str() else {
                continue;
            };
            let presence = &entry["presence"];
            let note = presence["note"]["message"].as_str().unwrap_or("");
            let said = super::html::parse(note, &Nobody, "").text;
            let doing_now = presence["activity"].as_str().unwrap_or("");
            let ooo = parse_out_of_office(presence);
            let status = match (activity(doing_now), ooo.is_some()) {
                (words, _) if !words.is_empty() && words != "Out of office" => words.to_owned(),
                (_, true) => "Out of office".to_owned(),
                _ => said,
            };
            let owned = id == self.me() || self.state().partners.contains_key(id);
            self.ctx.presence(id, presence_of(presence["availability"].as_str().unwrap_or("")), &status, owned);
            let kind = doing(doing_now);
            self.ctx.activity(id, "", kind.map(|kind| Activity { kind, name: String::new(), place: String::new(), conversation: None }), owned);
            self.save_away(id, ooo);
        }
    }

    /// Asks for the presence of exactly the people the shell has on screen, together, and our
    /// own with them. Asked when what is watched changes and every couple of minutes while
    /// anyone is; the same list is not asked about twice within a minute.
    pub(super) fn watch(&self) -> Result<(), Fault> {
        let watching = self.ctx.watching();
        let mine = watching.people.iter().filter(|person| person.account() == self.ctx.account);
        let mut ids: Vec<String> = mine.map(|person| person.id.clone()).filter(|id| id.starts_with("8:")).collect();
        if ids.is_empty() {
            self.state().watched.clear();
            return Ok(());
        }
        ids.truncate(WATCHED);
        ids.push(self.me());
        ids.sort();
        ids.dedup();
        {
            let mut state = self.state();
            if state.watched == ids && state.watched_at.is_some_and(|at| at.elapsed() < AGAIN) {
                return Ok(());
            }
            state.watched.clone_from(&ids);
            state.watched_at = Some(Instant::now());
        }
        let asked: Vec<Value> = ids.iter().map(|id| json!({"mri": id})).collect();
        let url = format!("{}/v1/presence/getpresence/", self.place()?.presence);
        let found = self.call("POST", &url, Auth::Bearer(PRESENCE), Body::Json(&json!(asked)), true)?.body;
        self.presences(&found);
        Ok(())
    }

    /// Sets our own availability and/or status message note, with optional expiry (`clear_at`).
    /// Passing `Presence::Unknown` with `status: None` clears any forced availability ("Reset status")
    /// via `DELETE /v1/me/forceavailability/` so Teams resumes automatic presence.
    pub(super) fn set_presence(&self, presence: Presence, status: Option<&str>, clear_at: Option<i64>) -> Result<(), Fault> {
        let place = self.place()?;
        if presence == Presence::Unknown && status.is_none() {
            let url = format!("{}/v1/me/forceavailability/", place.presence);
            self.call("DELETE", &url, Auth::Bearer(PRESENCE), Body::None, false)?;
            let me = self.me();
            if !me.is_empty() {
                self.fetch_presence_for(&[me]);
            }
            return Ok(());
        }
        if presence != Presence::Unknown {
            let availability = match presence {
                Presence::Online => "Available",
                Presence::Idle => "Away",
                Presence::Dnd => "DoNotDisturb",
                Presence::Busy => "Busy",
                Presence::Offline => "Offline",
                Presence::Unknown => unreachable!(),
            };
            let url = format!("{}/v1/me/forceavailability/", place.presence);
            let mut body = json!({"availability": availability});
            if let Some(ts) = clear_at.filter(|&ts| ts > 0) {
                body["expiry"] = Value::String(iso_utc(ts));
            }
            self.call("PUT", &url, Auth::Bearer(PRESENCE), Body::Json(&body), false)?;
        }
        if let Some(note) = status {
            let url = format!("{}/v1/me/publishnote", place.presence);
            let expiry = clear_at
                .filter(|&ts| ts > 0)
                .map(iso_utc)
                .unwrap_or_else(|| "9999-12-30T23:00:00.000Z".to_owned());
            let body = json!({"message": note, "expiry": expiry});
            self.call("PUT", &url, Auth::Bearer(PRESENCE), Body::Json(&body), false)?;
        }
        Ok(())
    }

    /// Reports in-call / presenting presence to Teams when a call is active, or clears forced
    /// call presence on hangup so Teams resumes automatic presence.
    #[allow(dead_code)]
    pub(super) fn report_call_presence(&self, in_call: bool, presenting: bool) -> Result<(), Fault> {
        let place = self.place()?;
        let url = format!("{}/v1/me/forceavailability/", place.presence);
        if in_call {
            let body = if presenting {
                json!({"availability": "DoNotDisturb", "activity": "Presenting"})
            } else {
                json!({"availability": "Busy", "activity": "InACall"})
            };
            self.call("PUT", &url, Auth::Bearer(PRESENCE), Body::Json(&body), false)?;
        } else {
            self.call("DELETE", &url, Auth::Bearer(PRESENCE), Body::None, false)?;
        }
        Ok(())
    }
}

/// A presence note is read for its words alone: it names nobody and carries no pictures.
pub(super) struct Nobody;

impl super::html::Lookup for Nobody {
    fn mention(&self, _: &str) -> Option<(protocol::MentionKind, String)> {
        None
    }

    fn hosted(&self, _: &str) -> bool {
        false
    }

    fn emoji(&self, _: &str, _: &str) -> Option<MediaFile> {
        None
    }
}

/// Escapes plain text before writing Exchange/Graph HTML automatic reply fields.
fn escape_ooo_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_is_one_of_five_states() {
        assert_eq!(presence_of("Available"), Presence::Online);
        assert_eq!((presence_of("Away"), presence_of("BeRightBack"), presence_of("AvailableIdle")), (Presence::Idle, Presence::Idle, Presence::Idle));
        assert_eq!((presence_of("Busy"), presence_of("DoNotDisturb")), (Presence::Busy, Presence::Dnd));
        assert_eq!((doing("InAMeeting"), doing("Presenting"), doing("Available")), (Some(ActivityKind::InMeeting), Some(ActivityKind::Presenting), None));
        assert_eq!((presence_of("Offline"), presence_of("PresenceUnknown")), (Presence::Offline, Presence::Unknown));
        assert_eq!((activity("InACall"), activity("Available")), ("In a call", ""));
    }

    #[test]
    fn people_are_read_from_whichever_list_the_middle_tier_answers_with() {
        let ana = json!({"mri": "8:orgid:ana"});
        assert_eq!(Teams::listed(&json!({"type": "x", "value": [ana]})).len(), 1);
        assert_eq!(Teams::listed(&json!({"resolvedUsers": [ana, ana]})).len(), 2);
        assert_eq!(Teams::listed(&json!([ana])).len(), 1);
        assert!(Teams::listed(&json!({"value": null})).is_empty());
        assert_eq!(Teams::link("ana@contoso.com"), "msteams:/l/chat/0/0?users=ana%40contoso.com");
        assert_eq!(Teams::link(""), "");
    }

    #[test]
    fn out_of_office_is_parsed_from_both_note_and_settings_shapes() {
        let note_shape = json!({
            "activity": "OutOfOffice",
            "outOfOfficeNote": {
                "message": "<p>Back Monday, ask Ben</p>",
                "expiry": "2026-10-19T08:00:00Z"
            }
        });
        assert_eq!(parse_out_of_office(&note_shape), Some(("Back Monday, ask Ben".to_owned(), Some(1_792_396_800))));

        let settings_shape = json!({
            "activity": "Away",
            "outOfOfficeSettings": {
                "isOutOfOffice": true,
                "message": "On vacation until Friday",
                "scheduledEndDateTime": {"dateTime": "2026-10-23T17:00:00Z", "timeZone": "UTC"}
            }
        });
        assert_eq!(parse_out_of_office(&settings_shape), Some(("On vacation until Friday".to_owned(), Some(1_792_774_800))));

        let cal_only = json!({"activity": "Available", "calendarData": {"isOutOfOffice": true}});
        assert_eq!(parse_out_of_office(&cal_only), Some(("Out of office".to_owned(), None)));

        let not_ooo = json!({"activity": "Available", "outOfOfficeSettings": {"isOutOfOffice": false}});
        assert_eq!(parse_out_of_office(&not_ooo), None);

        let disabled_with_saved_msg = json!({
            "activity": "Available",
            "outOfOfficeSettings": {
                "isOutOfOffice": false,
                "message": "Old saved vacation reply",
                "expirationDateTime": "2026-10-23T17:00:00Z"
            }
        });
        assert_eq!(parse_out_of_office(&disabled_with_saved_msg), None);

        let graph = json!({
            "status": "scheduled",
            "externalAudience": "all",
            "internalReplyMessage": "<p>Out this week</p>",
            "externalReplyMessage": "<p>External reply</p>",
            "scheduledStartDateTime": {"dateTime": "2026-10-19T08:00:00Z", "timeZone": "UTC"},
            "scheduledEndDateTime": {"dateTime": "2026-10-23T17:00:00Z", "timeZone": "UTC"}
        });
        let parsed = parse_automatic_replies(&graph);
        assert!(parsed.enabled && parsed.send_external);
        assert_eq!(parsed.message, "Out this week");
        assert_eq!(parsed.external_message, "External reply");
        assert_eq!(parsed.start, Some(1_792_396_800));
        assert_eq!(parsed.end, Some(1_792_774_800));
    }

    #[test]
    fn oid_and_timezone_offsets_are_parsed_for_windows_and_iana_names() {
        assert_eq!(oid_of("8:orgid:00000000-0000-4000-8000-0000000000a1"), Some("00000000-0000-4000-8000-0000000000a1"));
        assert_eq!(oid_of("28:bot-id"), None);
        assert_eq!(oid_of("8:orgid:bad/path"), None);
        assert_eq!(timezone_offset_secs("GMT Standard Time"), Some(0));
        assert_eq!(timezone_offset_secs("Eastern Standard Time"), Some(-5 * 3600));
        assert_eq!(timezone_offset_secs("India Standard Time"), Some(5 * 3600 + 1800));
        assert_eq!(timezone_offset_secs("Europe/Berlin"), Some(3600));
        assert_eq!(timezone_offset_secs("UTC+05:30"), Some(5 * 3600 + 1800));
        assert_eq!(timezone_offset_secs("UTC-8"), Some(-8 * 3600));
        assert_eq!(timezone_offset_secs("UTC+14:00"), Some(14 * 3600));
        assert_eq!(timezone_offset_secs("UTC+14:30"), None);
        assert_eq!(timezone_offset_secs("UTC+-5"), None);
        assert_eq!(timezone_offset_secs("UTC++5"), None);
        assert_eq!(timezone_offset_secs("Mars/Olympus"), None);
    }
}
