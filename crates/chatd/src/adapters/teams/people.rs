//! People in a tenant: who wrote a message, who a chat is with, who is online. The directory is
//! searched and looked up by id, never listed.

use super::{escaped, Auth, Body, Fault, Teams, PRESENCE, REMEMBERED, SPACES};
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

    /// One window of a conversation's members, in the order Teams lists them.
    pub(super) fn members(&self, conversation: &str, offset: u32, limit: u32) -> Result<Members, Fault> {
        let thread = super::thread_of(conversation);
        let described = self.chat("GET", &format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread)), Body::None, true)?.body;
        let all: Vec<(&str, &str)> = described["members"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|member| Some((member["id"].as_str()?, member["friendlyName"].as_str().unwrap_or(""))))
            .collect();
        let window: Vec<(&str, &str)> = all.iter().skip(offset as usize).take(limit as usize).copied().collect();
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
            // Teams' roster has no sections.
            sections: vec![MemberSection {
                name: "Members".to_owned(),
                count: all.len() as u32,
            }],
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

    /// One person's card: what their organization says of them.
    pub(super) fn profile(&self, person: &str) -> Result<About, Fault> {
        let path = "/beta/users/fetchShortProfile?isMailAddress=false&canBeSmtpAddress=false&enableGuest=true\
                    &includeIBBarredUsers=true&skypeTeamsInfo=true&includeBots=true";
        let found = self.middle(path, Body::Json(&json!([person])))?;
        let learned = self.learn(Teams::listed(&found));
        let known = learned.into_iter().find(|(id, _)| id == person).map(|(_, known)| known).or_else(|| self.cached(person));
        let known = known.unwrap_or_default();
        let (away, away_until) = self.state().away.get(person).cloned().unwrap_or_default();
        Ok(About {
            title: known.title,
            department: known.department,
            contacts: Some(known.mail).filter(|mail| !mail.is_empty()).map(|value| Contact { kind: ContactKind::Email, value, masked: false }).into_iter().collect(),
            away,
            away_until,
            ..About::default()
        })
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
            // The status line: what they are doing, else out of office, else their note.
            // Unverified: the out-of-office fields' names, read off the web client's answer.
            let oof = presence["outOfOfficeNote"]["message"].as_str().map(|note| super::html::parse(note, &Nobody, "").text).filter(|note| !note.is_empty());
            let away = presence["calendarData"]["isOutOfOffice"] == true || oof.is_some() || doing_now == "OutOfOffice";
            let status = match (activity(doing_now), away) {
                (words, _) if !words.is_empty() && words != "Out of office" => words.to_owned(),
                (_, true) => "Out of office".to_owned(),
                _ => said,
            };
            let owned = id == self.me() || self.state().partners.contains_key(id);
            self.ctx.presence(id, presence_of(presence["availability"].as_str().unwrap_or("")), &status, owned);
            let kind = doing(doing_now);
            self.ctx.activity(id, "", kind.map(|kind| Activity { kind, name: String::new(), place: String::new(), conversation: None }), owned);
            let mut state = self.state();
            if away {
                if state.away.len() >= AWAY_KEPT {
                    state.away.clear();
                }
                let until = presence["outOfOfficeNote"]["expiry"].as_str().map(super::seconds).filter(|until| *until > 0);
                state.away.insert(id.to_owned(), (oof.unwrap_or_else(|| "Out of office".to_owned()), until));
            } else {
                state.away.remove(id);
            }
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

    /// Sets our own availability. A status note is not written: none of the clients read shows
    /// how Teams wants one, so it is refused rather than guessed at.
    pub(super) fn set_presence(&self, presence: Presence, status: Option<&str>) -> Result<(), Fault> {
        let refuse = |message: &str| Fault::Refused {
            status: 400,
            message: message.to_owned(),
        };
        if status.is_some_and(|status| !status.is_empty()) {
            return Err(refuse("a status message is set in Teams itself"));
        }
        let availability = match presence {
            Presence::Online => "Available",
            Presence::Idle => "Away",
            Presence::Dnd => "DoNotDisturb",
            Presence::Busy => "Busy",
            Presence::Offline => "Offline",
            Presence::Unknown => return Err(refuse("Teams decides that one itself")),
        };
        let url = format!("{}/v1/me/forceavailability/", self.place()?.presence);
        self.call("PUT", &url, Auth::Bearer(PRESENCE), Body::Json(&json!({"availability": availability})), false)?;
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
}
