//! What answers for a demo account: an adapter with no network behind it. It serves the commands
//! the shell's requests become, with the same `Context` calls a real adapter makes, so the shell
//! can be clicked through and the server's own tests have a network that does as it is told.
//!
//! - `TINYCHAT_DEMO_VOICE` (`host:port`): a voice server to hand mediad when a call starts.
//!   Without it a call gets no voice session, as on a network that never answers.
//! - `TINYCHAT_DEMO_ANSWER_MS`: how long the other side of a direct call takes to pick up
//!   (4000). Only with a voice server; without one nobody answers.
//! - `TINYCHAT_DEMO_SIGN_OUT_MS`: how long ending the session on the "network" takes (150).
//! - `TINYCHAT_DEMO_UPLOAD_MS`: how long each quarter of a file upload takes (60), so a test
//!   can cancel one on its way.

use super::Who;
use crate::adapters::{self, About, Adapter, CatalogPage, Command, Context, ConversationAbout, Download, Members, Refusal, SharedPage, SpaceAbout};
use crate::hub::{lock, now, Hub};
use crate::store::{ConversationRow, Filing, PreviewLine};
use protocol::{
    AccountKey, Attachment, CallSupport, CatalogKind, Contact, ContactKind, ConversationKind, Dave, Key, Mark,
    MediaFile, MemberSection, Message, Network, Notify, Occupant, Outgoing, Participant, Presence, Reaction,
    RelateAction, Relation, ReplyTo, SharedKind, VoiceJoin, Warning,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Demo {
    ctx: Context,
    hub: Arc<Hub>,
    me: Who,
    /// Messages accepted so far, for their ids.
    made: AtomicU64,
    /// The conversation of the call we are in or ringing.
    call: Mutex<Option<String>>,
    /// Uploads the user cancelled, by client id, until their thread sees it.
    cancelled: Mutex<HashSet<String>>,
}

fn millis(name: &str, default: u64) -> Duration {
    Duration::from_millis(std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default))
}

/// What people say about themselves, where they are and the Spaces they are in, by person.
const BIOS: &[&str] = &[
    "Bryophytes, trail maps and the occasional pine marten.",
    "I fix cameras and then point them at logs.",
    "Field recorder. Ask me about lichens before coffee at your own risk.",
    "",
    "Herbarium volunteer. Handwriting decipherer.",
];
const ZONES: &[(&str, i32)] = &[("Europe/Lisbon", 3_600), ("America/New_York", -14_400), ("Asia/Tokyo", 32_400), ("Europe/Oslo", 7_200)];

pub fn start(account: AccountKey, hub: Arc<Hub>, me: Who) -> std::io::Result<Adapter> {
    let (adapter, ctx, inbox) = adapters::local(account, hub.clone());
    std::thread::Builder::new().name("demo-adapter".into()).stack_size(512 * 1024).spawn(move || {
        let demo = Arc::new(Demo {
            ctx: ctx.clone(),
            hub,
            me,
            made: AtomicU64::new(0),
            call: Mutex::new(None),
            cancelled: Mutex::new(HashSet::new()),
        });
        adapters::serve_commands(&ctx, inbox, Duration::ZERO, |_| {}, |command| Demo::serve(&demo, command));
    })?;
    Ok(adapter)
}

impl Demo {
    fn key(&self, id: &str) -> Key {
        self.ctx.key(id)
    }

    /// An id for a message the "network" accepted, in the network's own form, newer than any
    /// before it.
    fn message_id(&self, sent: i64) -> String {
        let made = self.made.fetch_add(1, Ordering::Relaxed);
        match self.ctx.account.network {
            Network::Slack => format!("{sent}.{:06}", 900_000 + made),
            _ => (1_300_000_002_000_000_000 + made).to_string(),
        }
    }

    /// The number at the end of a person's id, which the seed also ends their direct
    /// conversation's id with.
    fn direct_id(&self, person_id: &str) -> String {
        let number = &person_id[person_id.len().saturating_sub(3)..];
        match self.ctx.account.network {
            // Slack's are numbered from 0, by place in the cast.
            Network::Slack => {
                let at = number.parse::<u32>().unwrap_or(1).saturating_sub(1);
                format!("D0{}DIRECT{at:03}", &self.ctx.account.account[2..3])
            }
            // A Telegram chat with a person is named by the person.
            Network::Telegram => person_id.to_owned(),
            Network::Teams => format!("19:{}_robin@unq.gbl.spaces", number.parse::<u32>().unwrap_or(0)),
            Network::Discord => format!("500000000000000{number}"),
        }
    }

    fn send(&self, conversation_id: &str, client_id: &str, text: String, reply_to: Option<String>) {
        self.ctx.sent(conversation_id, client_id, Ok(self.accepted(conversation_id, text, reply_to, Vec::new())));
    }

    /// Our message as the network would echo it.
    fn accepted(&self, conversation_id: &str, text: String, reply_to: Option<String>, attachments: Vec<Attachment>) -> Message {
        let sent = now();
        let mut message = Message::plain(self.message_id(sent), "", text, sent, true);
        message.attachments = attachments;
        message.sender = self.me.sender();
        let quoted = reply_to.and_then(|id| self.ctx.stored_message(conversation_id, &id).ok().flatten());
        message.reply_to = quoted.map(|quoted| ReplyTo {
            id: quoted.id,
            person: (!quoted.sender.id.is_empty()).then(|| self.key(&quoted.sender.id)),
            sender: quoted.sender.name,
            snippet: quoted.text.chars().take(120).collect(),
            deleted: quoted.deleted,
        });
        message
    }

    /// Files "upload" from their paths in quarters, read from disk in parts as a real upload
    /// streams them, telling the progress as they go. A cancel stops them before the next part.
    fn send_with(demo: &Arc<Demo>, conversation_id: String, client_id: String, text: String, reply_to: Option<String>, with: Outgoing) {
        let demo = demo.clone();
        let spawned = std::thread::Builder::new().name("demo-upload".into()).stack_size(256 * 1024).spawn(move || {
            use std::io::Read;
            let total: u64 = with.files.iter().map(|file| std::fs::metadata(&file.path).map_or(0, |meta| meta.len())).sum();
            let step = millis("TINYCHAT_DEMO_UPLOAD_MS", 60);
            let mut sent = 0u64;
            let mut part = vec![0u8; 64 * 1024];
            for file in &with.files {
                let Ok(mut reader) = std::fs::File::open(&file.path) else {
                    let name = std::path::Path::new(&file.path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                    demo.ctx.sent(&conversation_id, &client_id, Err(format!("{name} could not be read.")));
                    return;
                };
                let size = reader.metadata().map_or(0, |meta| meta.len());
                let quarter = size.div_ceil(4).max(1);
                let mut done = 0u64;
                while done < size {
                    if lock(&demo.cancelled).remove(&client_id) {
                        demo.ctx.sent(&conversation_id, &client_id, Err("Cancelled".to_owned()));
                        return;
                    }
                    let mut read = 0u64;
                    while read < quarter {
                        match reader.read(&mut part) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => read += n as u64,
                        }
                    }
                    done += read.max(1);
                    sent = (sent + read.max(1)).min(total);
                    demo.ctx.upload(&conversation_id, &client_id, sent, total);
                    if demo.ctx.pause(step) {
                        return;
                    }
                }
            }
            if lock(&demo.cancelled).remove(&client_id) {
                demo.ctx.sent(&conversation_id, &client_id, Err("Cancelled".to_owned()));
                return;
            }
            demo.ctx.upload(&conversation_id, &client_id, total, total);
            let attachments = with
                .files
                .iter()
                .enumerate()
                .map(|(at, file)| {
                    let name = if file.name.is_empty() {
                        std::path::Path::new(&file.path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
                    } else {
                        file.name.clone()
                    };
                    let bytes = std::fs::metadata(&file.path).map_or(0, |meta| meta.len());
                    let kind = match std::path::Path::new(&file.path).extension().and_then(|extension| extension.to_str()) {
                        Some("png" | "jpg" | "jpeg" | "webp") => protocol::MediaKind::Image,
                        _ => protocol::MediaKind::File,
                    };
                    let id = format!("sent-{client_id}-{at}");
                    // A paste staged in outbox/ is the network's now: the demo keeps its copy in
                    // the media cache, as a network's would come back, and the outbox copy goes.
                    let mut path = file.path.clone();
                    if let Ok((staged, true)) = crate::blobs::upload::allowed(&file.path) {
                        let kept = crate::blobs::path_of(&protocol::support_dir().join("media"), &demo.ctx.account, &id);
                        let copied = kept.parent().is_some_and(|dir| std::fs::create_dir_all(dir).is_ok()) && std::fs::copy(&staged, &kept).is_ok();
                        if copied {
                            path = kept.to_string_lossy().into_owned();
                        }
                    }
                    let mut attached = Attachment::new(name, MediaFile { bytes, path: Some(path), ..MediaFile::new(id, kind) });
                    attached.spoiler = file.spoiler;
                    attached
                })
                .collect();
            let message = demo.accepted(&conversation_id, text, reply_to, attachments);
            demo.ctx.sent(&conversation_id, &client_id, Ok(message));
            crate::blobs::upload::discard(&with.files);
        });
        if let Err(err) = spawned {
            eprintln!("chatd: demo: upload: {err}");
        }
    }

    /// Our reaction as the network would report it. A custom emoji is named by its file's id.
    fn reaction(&self, emoji: &str) -> Reaction {
        let custom = self.hub.store().media(&self.ctx.account, &[emoji.to_owned()]).ok().and_then(|mut known| known.pop());
        match custom {
            Some((file, _)) => Reaction {
                emoji: format!(":{}:", emoji.trim_start_matches("emoji-")),
                file: Some(file),
                count: 1,
                mine: true,
            },
            None => Reaction {
                emoji: emoji.to_owned(),
                file: None,
                count: 1,
                mine: true,
            },
        }
    }

    fn edit(&self, conversation_id: &str, message_id: &str, text: String) -> Result<(), Refusal> {
        let stored = self.ctx.stored_message(conversation_id, message_id).ok().flatten();
        let mut message = stored.ok_or_else(|| Refusal::Refused("That message is gone.".to_owned()))?;
        message.text = text;
        message.spans.clear();
        message.edited = true;
        self.ctx.message(conversation_id, &message).map_err(|err| Refusal::Refused(err.to_string()))
    }

    /// A person's card as each network would fill it, typed.
    fn about(&self, person_id: &str, space_id: Option<&str>) -> About {
        let pick = person_id.bytes().fold(0usize, |sum, byte| sum.wrapping_mul(31).wrapping_add(byte as usize));
        let spaces = self.hub.store().spaces(Some(self.ctx.account.network)).unwrap_or_default();
        let ours: Vec<String> = spaces.into_iter().filter(|space| space.key.account() == self.ctx.account).map(|space| space.name).collect();
        let person = self.hub.store().person(&self.key(person_id)).ok().flatten();
        // The zone the list already shows them in, so the two agree.
        let (zone, offset) = match person.as_ref().and_then(|person| person.utc_offset) {
            Some(known) => ZONES.iter().find(|(_, offset)| *offset == known).map_or(("", known), |(zone, _)| (*zone, known)),
            None => ZONES[pick % ZONES.len()],
        };
        let name = person.as_ref().map_or_else(String::new, |person| person.name.clone());
        let first = name.split_whitespace().next().unwrap_or("").to_lowercase();
        let bot = person.as_ref().is_some_and(|person| person.bot);
        let friend = person.as_ref().is_some_and(|person| person.relation == Relation::Friend);
        // An app's About is what it says it does, not a person's line.
        let mut about = About {
            bio: if bot { "Answers commands. Send /help to see them.".to_owned() } else { BIOS[pick % BIOS.len()].to_owned() },
            ..About::default()
        };
        match self.ctx.account.network {
            Network::Discord => {
                // Discord's ids carry the date the account was made.
                about.since = Some(1_500_000_000 + (pick % 200_000_000) as i64);
                about.mutual_spaces = ours.iter().take(1 + pick % 4).cloned().collect();
                about.mutual_total = (about.mutual_spaces.len() + pick % 3) as u32;
                about.mutual_friends = (pick % 9) as u32;
                about.accent = Some([0xd9_82_4a, 0x4c_9a_6a, 0x5b_6e_e1, 0xb5_4a_7a][pick % 4]);
                if !bot {
                    about.pronouns = ["she/her", "he/him", "they/them", ""][pick % 4].to_owned();
                }
                if friend && !bot {
                    about.friends_since = Some(1_620_000_000 + (pick % 50_000_000) as i64);
                }
                about.note = if pick % 3 == 0 { "Met at the spring foray. Knows lichens.".to_owned() } else { String::new() };
                if space_id.is_some() {
                    about.nickname = format!("{} 🌿", name.split_whitespace().next().unwrap_or(""));
                    about.roles = ["Trail lead", "Moderator", "Photographer"].iter().take(1 + pick % 3).map(|role| (*role).to_owned()).collect();
                    about.joined = Some(1_640_000_000 + (pick % 40_000_000) as i64);
                }
            }
            Network::Slack => {
                // Slack has no bio and no join date: the title is all it says of someone.
                about.bio = String::new();
                if !bot {
                    about.time_zone = zone.to_owned();
                    about.utc_offset = Some(offset);
                    about.title = person.as_ref().map_or_else(String::new, |person| person.title.clone());
                    about.pronouns = ["she/her", "he/him", "they/them", ""][pick % 4].to_owned();
                    about.contacts = vec![
                        Contact { kind: ContactKind::Email, value: format!("{first}@harbor.example"), masked: false },
                        Contact { kind: ContactKind::Phone, value: format!("+44 7700 900{:03}", pick % 1000), masked: true },
                    ];
                }
                about.status_until = Some(now() + 3_600 * (1 + (pick % 5) as i64));
            }
            Network::Telegram => {
                // Neither bots nor Telegram's own account share a phone; no join date is told.
                let service = person.as_ref().is_some_and(|person| person.mark == Some(Mark::Verified));
                if !bot && !service {
                    about.contacts = vec![Contact { kind: ContactKind::Phone, value: format!("+351 912 345 {:03}", pick % 1000), masked: true }];
                }
                if service {
                    about.bio = String::new();
                }
                // An exact last seen only for someone who shows it: who hides it has Telegram's
                // rough words ("last seen recently") and nothing more.
                let hidden = person.as_ref().is_some_and(|person| person.status.to_lowercase().starts_with("last seen"));
                let around = matches!(self.ctx.presence_of(person_id), Presence::Online);
                if !bot && !service && !hidden && !around {
                    about.last_seen = Some(now() - 600 * (1 + (pick % 30) as i64));
                }
                about.mutual_total = if bot || service { 0 } else { (pick % 4) as u32 };
                if person.as_ref().is_some_and(|person| person.mark == Some(Mark::Scam)) {
                    about.warning = Some(Warning::Scam);
                    about.bio = "Official giveaway. Limited time!".to_owned();
                }
            }
            Network::Teams => {
                about.bio = String::new();
                about.title = person.as_ref().map_or_else(String::new, |person| person.title.clone());
                about.department = "Engineering".to_owned();
                about.contacts = vec![Contact { kind: ContactKind::Email, value: person.as_ref().map_or_else(String::new, |person| person.handle.clone()), masked: false }];
                about.utc_offset = person.as_ref().and_then(|person| person.utc_offset);
                if name.starts_with("Chloé") {
                    about.away = "At the design offsite. For onboarding, ask Robin.".to_owned();
                    // Back the Friday her status names: the banner and the status agree.
                    let day = now().div_euclid(86_400);
                    let to_friday = match (5 - (day + 4).rem_euclid(7)).rem_euclid(7) {
                        0 => 7,
                        days => days,
                    };
                    about.away_until = Some((day + to_friday) * 86_400 + 17 * 3_600);
                }
            }
        }
        about
    }

    fn conversation_about(&self, conversation_id: &str) -> Result<ConversationAbout, Refusal> {
        let row = self.ctx.known(conversation_id).ok().flatten().ok_or_else(|| Refusal::Refused("That conversation is gone.".to_owned()))?;
        let pick = conversation_id.bytes().fold(0usize, |sum, byte| sum.wrapping_mul(31).wrapping_add(byte as usize));
        let members = match row.kind {
            ConversationKind::Direct => 2,
            // A group's count is its own people, the ones its member list names.
            ConversationKind::Group => self.group_people(conversation_id).len() as u32,
            _ => 40 + (pick % 1_200) as u32,
        };
        Ok(ConversationAbout {
            description: if row.topic.is_empty() { String::new() } else { format!("{}\n\nPinned rules and links are in the pins.", row.topic) },
            members: Some(members),
            online: Some(members / 3),
            created: Some(1_600_000_000 + (pick % 100_000_000) as i64),
            creator: if row.kind.is_direct() { String::new() } else { "Ines Marchetti".to_owned() },
            notify: if row.muted { Notify::Nothing } else { Notify::All },
            pinned: Some((pick % 4) as u32),
            external: if self.ctx.account.network == Network::Slack && row.title == "customer-voice" { "Northwind Traders".to_owned() } else { String::new() },
            link: if self.ctx.account.network == Network::Telegram && !row.kind.is_direct() { format!("https://t.me/{}", row.title.to_lowercase().replace(' ', "_")) } else { String::new() },
        })
    }

    fn space_about(&self, space_id: &str) -> Result<SpaceAbout, Refusal> {
        let pick = space_id.bytes().fold(0usize, |sum, byte| sum.wrapping_mul(31).wrapping_add(byte as usize));
        let network = self.ctx.account.network;
        // Slack says nothing of a workspace but its name; Teams has no online count.
        Ok(SpaceAbout {
            description: if network == Network::Slack { String::new() } else { "Field trips, identification help and a lot of photos of moss.".to_owned() },
            members: (network != Network::Slack).then_some(200 + (pick % 2_000) as u32),
            online: (network == Network::Discord).then_some(20 + (pick % 200) as u32),
            nickname: if self.ctx.account.network == Network::Discord { "Robin 🍄".to_owned() } else { String::new() },
            roles: if self.ctx.account.network == Network::Discord { vec!["Member".to_owned(), "Trail lead".to_owned()] } else { Vec::new() },
            notify: Notify::Mentions,
        })
    }

    /// What is pinned: the cached messages that say so, newest first.
    fn pinned(&self, conversation_id: &str) -> Vec<Message> {
        let page = self.hub.store().messages(&self.key(conversation_id), &protocol::Page::Newest, 200, now());
        let messages = page.ok().flatten().map(|page| page.messages).unwrap_or_default();
        messages.into_iter().rev().filter(|message| message.pinned).take(protocol::PINNED_LIMIT).collect()
    }

    /// The "network's" own search of what was shared: the whole cache, said to be complete.
    fn search_shared(&self, conversation_id: &str, kind: SharedKind, before: Option<&str>, limit: u32) -> Result<SharedPage, Refusal> {
        let found = self.hub.store().shared(&self.key(conversation_id), kind, before, limit);
        match found {
            Ok(Some((items, more))) => {
                let total = (!more && before.is_none()).then_some(items.len() as u32);
                Ok(SharedPage { items, more, total })
            }
            Ok(None) => Err(Refusal::Refused("That conversation is gone.".to_owned())),
            Err(err) => Err(Refusal::Refused(err.to_string())),
        }
    }

    fn catalog(&self, kind: CatalogKind, query: &str, cursor: Option<&str>) -> CatalogPage {
        let items = super::polish::catalog(&self.ctx.account, kind);
        let ids: Vec<String> = items.iter().map(|(id, ..)| (*id).to_owned()).collect();
        let files: Vec<MediaFile> = self.hub.store().media(&self.ctx.account, &ids).unwrap_or_default().into_iter().map(|(file, _)| file).collect();
        let (mut items, cursor) = super::polish::page(&files, kind, items, query, cursor);
        if kind == CatalogKind::Gifs {
            // Sections are the empty query's: a search is one list. Telegram's are the saved ones.
            for item in &mut items {
                item.pack = match (query.is_empty(), self.ctx.account.network) {
                    (false, _) => String::new(),
                    (true, Network::Telegram) => "Saved".to_owned(),
                    (true, _) => std::mem::take(&mut item.pack),
                };
            }
        }
        CatalogPage { items, cursor }
    }

    /// Rows on screen with no preview: the "network" says what was said last.
    fn previews(&self, ids: &[String]) {
        for id in ids {
            let line = PreviewLine {
                id: format!("{}000", now()),
                text: "Calendar invite: kickoff on Monday".to_owned(),
                sender: "Ana Ruiz".to_owned(),
                mine: false,
                sent: self.ctx.known(id).ok().flatten().map_or_else(now, |row| row.updated.max(1)),
            };
            if let Err(err) = self.ctx.preview(id, &line) {
                eprintln!("chatd: demo: preview: {err}");
            }
        }
    }

    /// A group's people: us, whoever wrote in it, and whoever its title names.
    fn group_people(&self, conversation_id: &str) -> Vec<protocol::Person> {
        let owned = self.hub.store().people(Some(self.ctx.account.network), None, None, 0, protocol::PAGE_LIMIT);
        let mut people = owned.map(|(people, _)| people).unwrap_or_default();
        people.retain(|person| person.key.account() == self.ctx.account);
        let title = self.ctx.known(conversation_id).ok().flatten().map(|row| row.title).unwrap_or_default();
        let named: Vec<&str> = title.split(',').map(str::trim).collect();
        let page = self.hub.store().messages(&self.key(conversation_id), &protocol::Page::Newest, 200, now());
        let spoke: HashSet<String> = page.ok().flatten().map(|page| page.messages.into_iter().map(|message| message.sender.id).collect()).unwrap_or_default();
        people.retain(|person| {
            let first = person.name.split_whitespace().next().unwrap_or("");
            person.key.id == self.me.id || spoke.contains(&person.key.id) || named.iter().any(|name| *name == person.name || *name == first)
        });
        people
    }

    /// The people the account owns, as a member list: who is around, then who is not. A group
    /// lists only its own.
    fn members(&self, conversation_id: &str, offset: u32, limit: u32) -> Result<Members, Refusal> {
        let group = self.ctx.known(conversation_id).ok().flatten().is_some_and(|row| row.kind == ConversationKind::Group);
        let mut people = if group {
            self.group_people(conversation_id)
        } else {
            let owned = self.hub.store().people(Some(self.ctx.account.network), None, None, 0, protocol::PAGE_LIMIT);
            let (mut people, _) = owned.map_err(|err| Refusal::Refused(err.to_string()))?;
            people.retain(|person| person.key.account() == self.ctx.account);
            people
        };
        for person in &mut people {
            person.presence = self.ctx.presence_of(&person.key.id);
        }
        let around = |presence: Presence| matches!(presence, Presence::Online | Presence::Idle | Presence::Dnd);
        people.sort_by_key(|person| !around(person.presence));
        let online = people.iter().filter(|person| around(person.presence)).count() as u32;
        let sections = vec![
            MemberSection { name: "Online".to_owned(), count: online },
            MemberSection { name: "Offline".to_owned(), count: people.len() as u32 - online },
        ];
        let members = people.into_iter().skip(offset as usize).take(limit as usize).collect();
        Ok(Members { sections, members })
    }

    fn start_direct(&self, person_id: &str) -> Result<String, Refusal> {
        let id = self.direct_id(person_id);
        let gone = || Refusal::Refused("That person is not here any more.".to_owned());
        if self.ctx.known(&id).map_err(|_| gone())?.is_some() {
            return Ok(id);
        }
        let person = self.hub.store().person(&self.key(person_id)).ok().flatten().ok_or_else(gone)?;
        let row = ConversationRow {
            avatar: person.avatar.map(|file| file.id),
            peer: Some(person_id.to_owned()),
            call: if self.ctx.account.network == Network::Discord { CallSupport::Native } else { CallSupport::Handoff },
            ..ConversationRow::new(id.clone(), ConversationKind::Direct, person.name)
        };
        self.ctx.conversation(&row).map_err(|_| gone())?;
        Ok(id)
    }

    /// Puts us among a voice channel's occupants, or takes us out, as the network would show it.
    fn occupy(&self, conversation_id: &str, change: impl FnOnce(&mut Vec<Occupant>, Occupant)) {
        let mut occupants = self.hub.occupants(&self.key(conversation_id)).unwrap_or_default();
        let hidden = occupants.total.saturating_sub(occupants.people.len() as u32);
        let ours = self.key(&self.me.id);
        let at = occupants.people.iter().position(|occupant| occupant.person == ours);
        let me = at.map(|at| occupants.people.remove(at)).unwrap_or(Occupant {
            person: ours,
            name: self.me.name.clone(),
            muted: false,
            deafened: false,
            streaming: false,
        });
        change(&mut occupants.people, me);
        let total = occupants.people.len() as u32 + hidden;
        self.ctx.occupants(conversation_id, occupants.people, total);
    }

    fn call_join(demo: &Arc<Demo>, conversation_id: &str) {
        let Ok(Some(row)) = demo.ctx.known(conversation_id) else {
            return;
        };
        *demo.call.lock().unwrap() = Some(conversation_id.to_owned());
        let rings = row.kind != ConversationKind::Voice;
        if rings {
            demo.ctx.ringing_out(conversation_id);
        } else {
            demo.occupy(conversation_id, |people, me| people.push(me));
            let ours = demo.key(&demo.me.id);
            let others = demo.hub.occupants(&demo.key(conversation_id)).unwrap_or_default().people;
            let roster = others.into_iter().filter(|occupant| occupant.person != ours).map(|occupant| Participant {
                id: occupant.person.id,
                name: occupant.name,
                speaking: false,
                muted: occupant.muted,
                deafened: occupant.deafened,
            });
            demo.ctx.participants(conversation_id, roster.collect());
        }
        let Ok(endpoint) = std::env::var("TINYCHAT_DEMO_VOICE") else {
            return;
        };
        demo.ctx.voice(conversation_id, VoiceJoin {
            network: demo.ctx.account.network,
            endpoint,
            token: "demo-voice-token".to_owned(),
            session_id: "demo-session".to_owned(),
            user_id: demo.me.id.clone(),
            server_id: conversation_id.to_owned(),
            channel_id: conversation_id.to_owned(),
            dave: Dave { max_version: 1, required: false },
            muted: false,
            deafened: false,
        });
        if !rings {
            return;
        }
        // The other side picks up after a while, unless we hung up first.
        let (demo, conversation_id) = (demo.clone(), conversation_id.to_owned());
        let _ = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            if demo.ctx.pause(millis("TINYCHAT_DEMO_ANSWER_MS", 4_000)) {
                return;
            }
            if demo.call.lock().unwrap().as_deref() != Some(conversation_id.as_str()) {
                return;
            }
            demo.ctx.answered(&conversation_id);
            let peer = demo.ctx.known(&conversation_id).ok().flatten().and_then(|row| Some((row.peer?, row.title)));
            if let Some((id, name)) = peer {
                let them = Participant { id, name, speaking: false, muted: false, deafened: false };
                demo.ctx.participants(&conversation_id, vec![them]);
            }
        });
    }

    fn call_leave(&self, conversation_id: &str) {
        let mut call = self.call.lock().unwrap();
        if call.as_deref() == Some(conversation_id) {
            *call = None;
        }
        drop(call);
        if self.hub.occupants(&self.key(conversation_id)).is_some() {
            self.occupy(conversation_id, |_, _| {});
        }
    }

    fn serve(demo: &Arc<Demo>, command: Command) {
        match command {
            Command::Send { conversation_id, client_id, text, reply_to } => demo.send(&conversation_id, &client_id, text, reply_to),
            Command::React { conversation_id, message_id, emoji, on, reply } => {
                let reacted = demo.ctx.reacted(&conversation_id, &message_id, &demo.reaction(&emoji), on);
                let _ = reply.send(reacted.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Edit { conversation_id, message_id, text, reply } => {
                let _ = reply.send(demo.edit(&conversation_id, &message_id, text));
            }
            Command::Delete { conversation_id, message_id, reply } => {
                let deleted = demo.ctx.deleted(&conversation_id, &message_id);
                let _ = reply.send(deleted.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Relate { person_id, action, reply } => {
                let relation = match action {
                    RelateAction::Accept => Relation::Friend,
                    RelateAction::Block => Relation::Blocked,
                    RelateAction::Ignore | RelateAction::Cancel | RelateAction::Remove => Relation::None,
                };
                let related = demo.ctx.relation(&person_id, relation);
                let _ = reply.send(related.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::SetPresence { presence, status, reply } => {
                let (_, old) = demo.hub.shown(&demo.key(&demo.me.id));
                demo.ctx.presence(&demo.me.id, presence, status.as_deref().unwrap_or(&old), true);
                let _ = reply.send(Ok(()));
            }
            Command::Profile { person_id, reply } => drop(reply.send(Ok(demo.about(&person_id, None)))),
            Command::ProfileIn { person_id, space_id, reply } => drop(reply.send(Ok(demo.about(&person_id, Some(&space_id))))),
            Command::ConversationInfo { conversation_id, reply } => drop(reply.send(demo.conversation_about(&conversation_id))),
            Command::SpaceInfo { space_id, reply } => drop(reply.send(demo.space_about(&space_id))),
            Command::SetMute { conversation_id, notify, reply } => {
                let muted = demo.ctx.muted(&conversation_id, notify == Notify::Nothing);
                let _ = reply.send(muted.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Leave { conversation_id, reply } => {
                let left = demo.ctx.conversation_gone(&conversation_id);
                let _ = reply.send(left.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::CloseDirect { conversation_id, reply } => {
                let closed = demo.ctx.filing(&conversation_id, &Filing { hidden: true, ..Filing::default() });
                let _ = reply.send(closed.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Pinned { conversation_id, reply } => drop(reply.send(Ok(demo.pinned(&conversation_id)))),
            Command::SearchShared { conversation_id, kind, before, limit, reply } => {
                let _ = reply.send(demo.search_shared(&conversation_id, kind, before.as_deref(), limit));
            }
            Command::Catalog { kind, query, cursor, reply, .. } => drop(reply.send(Ok(demo.catalog(kind, &query, cursor.as_deref())))),
            Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
                Demo::send_with(demo, conversation_id, client_id, text, reply_to, *with);
            }
            Command::CancelSend { client_id, .. } => {
                lock(&demo.cancelled).insert(client_id);
            }
            Command::Previews(ids) => demo.previews(&ids),
            Command::Members { conversation_id, offset, limit, reply } => drop(reply.send(demo.members(&conversation_id, offset, limit))),
            Command::StartDirect { person_id, reply } => drop(reply.send(demo.start_direct(&person_id))),
            Command::SearchPeople(query) => demo.ctx.searched(&query),
            Command::Download { url, reply, .. } => drop(reply.send(Ok(Download { url, headers: Vec::new() }))),
            Command::Handoff { conversation_id, reply } => {
                let account = &demo.ctx.account.account;
                let link = match demo.ctx.account.network {
                    Network::Slack => format!("https://app.slack.com/huddle/{account}/{conversation_id}"),
                    _ => format!("https://discord.com/channels/@me/{conversation_id}"),
                };
                let _ = reply.send(Ok(link));
            }
            Command::CallJoin(conversation_id) => Demo::call_join(demo, &conversation_id),
            Command::CallLeave(conversation_id) => demo.call_leave(&conversation_id),
            Command::CallAudio { muted, deafened } => {
                let call = demo.call.lock().unwrap().clone();
                if let Some(conversation_id) = call.filter(|id| demo.hub.occupants(&demo.key(id)).is_some()) {
                    demo.occupy(&conversation_id, |people, me| people.push(Occupant { muted: muted || deafened, deafened, ..me }));
                }
            }
            Command::SignOut(reply) => {
                demo.ctx.pause(millis("TINYCHAT_DEMO_SIGN_OUT_MS", 150));
                let _ = reply.send(Ok(()));
            }
            // Nothing is behind these: what is stored is all there is.
            Command::Backfill(_)
            | Command::Warm(_)
            | Command::History { .. }
            | Command::HistoryAfter { .. }
            | Command::HistoryAround { .. }
            | Command::OpenSpace(_)
            | Command::OpenHome
            | Command::OpenThreads(_)
            | Command::MarkRead { .. }
            | Command::Typing(_)
            | Command::Watch(_)
            | Command::Stop => {}
        }
    }
}
