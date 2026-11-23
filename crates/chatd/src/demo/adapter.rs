//! What answers for a demo account: an adapter with no network behind it. It serves the commands
//! the shell's requests become, with the same `Context` calls a real adapter makes, so the shell
//! can be clicked through and the server's own tests have a network that does as it is told.
//!
//! - `TINYCLIENT_DEMO_VOICE` (`host:port`): a voice server to hand mediad when a call starts.
//!   Without it a call gets no voice session, as on a network that never answers.
//! - `TINYCLIENT_DEMO_ANSWER_MS`: how long the other side of a direct call takes to pick up
//!   (4000). Only with a voice server; without one nobody answers.
//! - `TINYCLIENT_DEMO_SIGN_OUT_MS`: how long ending the session on the "network" takes (150).
//! - `TINYCLIENT_DEMO_UPLOAD_MS`: how long each quarter of a file upload takes (60), so a test
//!   can cancel one on its way.

use super::Who;
use crate::adapters::{self, About, Adapter, CatalogPage, Command, Context, ConversationAbout, Download, Members, Refusal, SharedPage, SpaceAbout};
use crate::hub::{lock, now, Hub};
use crate::store::{ConversationRow, Filing, PreviewLine};
use protocol::{
    AccountKey, Attachment, CallSupport, CatalogKind, Contact, ContactKind, ConversationKind, Dave, Key, Mark,
    MediaFile, MemberSection, Message, Network, Notify, Occupant, Outgoing, Participant, Presence, Reaction,
    ReactionRules, RelateAction, Relation, ReplyTo, SharedKind, VoiceJoin, Warning, EmojiScope,
};
use crate::adapters::telegram::react::{rules_of as telegram_rules, Allowed};
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
            // A Signal chat with a person is named by the person.
            Network::Signal | Network::Messages | Network::Tiny => person_id.to_owned(),
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
            thumb: None,
            story: false,
        });
        message
    }

    /// Files "upload" from their paths in quarters, read from disk in parts as a real upload
    /// streams them, telling the progress as they go. A cancel stops them before the next part.
    fn send_with(demo: &Arc<Demo>, conversation_id: String, client_id: String, text: String, reply_to: Option<String>, with: Outgoing) {
        if let Some(draft) = with.poll {
            let question = draft.question.trim().to_owned();
            let options: Vec<String> = draft.options.into_iter().map(|o| o.trim().to_owned()).filter(|o| !o.is_empty()).collect();
            if question.is_empty() || options.len() < 2 {
                demo.ctx.sent(&conversation_id, &client_id, Err("A poll needs a question and at least two options.".to_owned()));
                return;
            }
            let mut poll = protocol::Poll::open(question.clone(), options, draft.multiple);
            poll.anonymous = draft.anonymous;
            if let Some(secs) = draft.duration_secs.filter(|&s| s > 0) {
                poll.closes_at = Some(now() + i64::from(secs));
            }
            let mut message = demo.accepted(&conversation_id, format!("Poll: {question}"), reply_to, Vec::new());
            message.poll = Some(Box::new(poll));
            demo.ctx.sent(&conversation_id, &client_id, Ok(message));
            return;
        }
        let demo = demo.clone();
        let spawned = std::thread::Builder::new().name("demo-upload".into()).stack_size(256 * 1024).spawn(move || {
            use std::io::Read;
            let total: u64 = with.files.iter().map(|file| std::fs::metadata(&file.path).map_or(0, |meta| meta.len())).sum();
            let step = millis("TINYCLIENT_DEMO_UPLOAD_MS", 60);
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
                    attached.view_once = file.view_once;
                    attached
                })
                .collect();
            let mut message = demo.accepted(&conversation_id, text, reply_to, attachments);
            message.importance = with.importance;
            message.effect = with.effect.clone();
            message.spans = with.spans.clone();
            demo.ctx.sent(&conversation_id, &client_id, Ok(message));
            crate::blobs::upload::discard(&with.files);
        });
        if let Err(err) = spawned {
            tracing::warn!("chatd: demo: upload: {err}");
        }
    }

    /// Our reaction as the network would report it. A custom emoji is named by its file's id.
    /// A super reaction is counted on its own, not as a normal one.
    fn reaction(&self, emoji: &str, burst: bool) -> Reaction {
        let custom = self.hub.store().media(&self.ctx.account, &[emoji.to_owned()]).ok().and_then(|mut known| known.pop());
        let (emoji, file) = match custom {
            Some((file, _)) => (format!(":{}:", emoji.trim_start_matches("emoji-")), Some(file)),
            None => (emoji.to_owned(), None),
        };
        Reaction {
            emoji,
            file,
            count: if burst { 0 } else { 1 },
            mine: !burst,
            who: Vec::new(),
            burst: u32::from(burst),
            mine_burst: burst,
            burst_colors: Vec::new(),
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
            // Signal tells a linked device nothing more about a person.
            Network::Signal | Network::Messages | Network::Tiny => {}
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
            disappearing: row.disappearing,
            bookmarks: Vec::new(),
            pending_requests: 0,
            ..ConversationAbout::default()
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
            can_change_nickname: self.ctx.account.network == Network::Discord,
            ..SpaceAbout::default()
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

    /// Rows on screen with no preview. A stored message is that line. A chat with none
    /// stays "No messages yet" instead of a line the network never sent.
    fn previews(&self, ids: &[String]) {
        for id in ids {
            let sent = self.ctx.known(id).ok().flatten().map_or_else(now, |row| row.updated.max(1));
            let line = match self.hub.store().newest_line(&self.key(id)) {
                Ok(Some(mut line)) => {
                    line.sent = line.sent.max(sent);
                    line
                }
                Ok(None) => PreviewLine { id: String::new(), text: "No messages yet".to_owned(), sender: String::new(), mine: false, sent, expires_at: None },
                Err(err) => {
                    tracing::warn!("chatd: demo: preview: {err}");
                    continue;
                }
            };
            if let Err(err) = self.ctx.preview(id, &line) {
                tracing::warn!("chatd: demo: preview: {err}");
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
        Ok(Members { pending: false, sections, members })
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
            call: match self.ctx.account.network {
                Network::Discord => CallSupport::Native,
                Network::Signal | Network::Tiny => CallSupport::None,
                _ => CallSupport::Handoff,
            },
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
            avatar: None,
            muted: false,
            deafened: false,
            server_muted: false,
            server_deafened: false,
            suppressed: false,
            streaming: false,
            camera: false,
            speaking: false,
            hand_raised: false,
            is_channel: false,
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
                ..Participant::default()
            });
            demo.ctx.participants(conversation_id, roster.collect());
        }
        let Ok(endpoint) = std::env::var("TINYCLIENT_DEMO_VOICE") else {
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
            stream: false,
            watch: false,
            stream_owner: None,
            group_id: String::new(),
        });
        if !rings {
            return;
        }
        // The other side picks up after a while, unless we hung up first.
        let (demo, conversation_id) = (demo.clone(), conversation_id.to_owned());
        let _ = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            if demo.ctx.pause(millis("TINYCLIENT_DEMO_ANSWER_MS", 4_000)) {
                return;
            }
            if demo.call.lock().unwrap().as_deref() != Some(conversation_id.as_str()) {
                return;
            }
            demo.ctx.answered(&conversation_id);
            let peer = demo.ctx.known(&conversation_id).ok().flatten().and_then(|row| Some((row.peer?, row.title)));
            if let Some((id, name)) = peer {
                let them = Participant { id, name, ..Participant::default() };
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

    /// Each network's real rules, with the restrictions a real account meets: on Telegram a
    /// group whose admins picked five reactions, a channel with three and an archived one with
    /// none; on Discord read-only channels where Add Reactions is off, so only reactions
    /// already there can be added to; Signal's one reaction a person.
    fn reaction_rules(&self, conversation_id: &str) -> ReactionRules {
        let row = self.ctx.known(conversation_id).ok().flatten();
        match self.ctx.account.network {
            Network::Telegram => {
                let standard: Vec<(String, bool)> = super::telegram::REACTIONS.iter().map(|emoji| ((*emoji).to_owned(), false)).collect();
                let some = |list: &[&str]| Allowed::Some(list.iter().map(|emoji| (*emoji).to_owned()).collect());
                let allowed = match conversation_id {
                    "g8001" => some(&["👍", "❤", "🔥", "🎉", "😁"]),
                    super::telegram::CHANNEL => some(&["👍", "❤", "🔥"]),
                    "c9002" => Allowed::None,
                    _ => Allowed::All { custom: true },
                };
                telegram_rules(&standard, allowed, false, None)
            }
            Network::Discord => {
                let direct = row.as_ref().is_none_or(|row| row.space.is_none());
                let join_only = row.is_some_and(|row| row.read_only);
                ReactionRules {
                    custom: if direct { EmojiScope::None } else { EmojiScope::Space },
                    join_only,
                    distinct: 20,
                    // The demo Discord account has super reactions. A read-only channel still cannot start one.
                    burst: !join_only,
                    ..ReactionRules::default()
                }
            }
            Network::Slack => crate::adapters::slack::reaction_rules(),
            Network::Teams => crate::adapters::teams::message::reaction_rules(),
            Network::Signal => crate::adapters::signal::reaction_rules(),
            Network::Messages => crate::adapters::messages::reaction_rules(),
            Network::Tiny => crate::adapters::messages::reaction_rules(),
        }
    }

    fn serve(demo: &Arc<Demo>, command: Command) {
        match command {
            Command::Send { conversation_id, client_id, text, reply_to } => demo.send(&conversation_id, &client_id, text, reply_to),
            Command::React { conversation_id, message_id, emoji, on, burst, reply } => {
                let reacted = demo.ctx.reacted(&conversation_id, &message_id, &demo.reaction(&emoji, burst), on);
                let _ = reply.send(reacted.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Edit { conversation_id, message_id, text, spans: _, suppress_embeds: _, keep_attachments: _, reply } => {
                let _ = reply.send(demo.edit(&conversation_id, &message_id, text));
            }
            Command::EditMedia { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Delete { conversation_id, message_id, scope: _, reply } => {
                let deleted = demo.ctx.deleted(&conversation_id, &message_id);
                let _ = reply.send(deleted.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Vote { conversation_id, message_id, votes, reply } => {
                let outcome = (|| -> Result<(), Refusal> {
                    let stored = demo.ctx.stored_message(&conversation_id, &message_id).ok().flatten();
                    let mut message = stored.ok_or_else(|| Refusal::Refused("That message is gone.".to_owned()))?;
                    let poll = message.poll.as_deref_mut().ok_or_else(|| Refusal::Refused("That is not a poll.".to_owned()))?;
                    match votes {
                        None => {
                            if !message.outgoing {
                                return Err(Refusal::Refused("Only the person who started this poll can end it.".to_owned()));
                            }
                            poll.closed = true;
                        }
                        Some(choices) => {
                            if poll.closed {
                                return Err(Refusal::Refused("This poll has ended.".to_owned()));
                            }
                            poll.apply_vote(&demo.me.id, choices, &demo.me.id);
                        }
                    }
                    demo.ctx.message(&conversation_id, &message).map_err(|err| Refusal::Refused(err.to_string()))
                })();
                let _ = reply.send(outcome);
            }
            Command::Relate { person_id, action, reply } => {
                let relation = match action {
                    RelateAction::Add => Relation::Outgoing,
                    RelateAction::Accept => Relation::Friend,
                    RelateAction::Block => Relation::Blocked,
                    RelateAction::IgnoreUser => Relation::Ignored,
                    RelateAction::Ignore | RelateAction::Cancel | RelateAction::Remove | RelateAction::Unblock | RelateAction::UnignoreUser => {
                        Relation::None
                    }
                };
                let related = demo.ctx.relation(&person_id, relation);
                let _ = reply.send(related.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Report { target, reason: _, note: _, block, reply } => {
                if block {
                    let person_id = match &target {
                        protocol::ReportTarget::Person(key) => Some(key.id.clone()),
                        protocol::ReportTarget::Message { conversation, message_id } => demo
                            .hub
                            .store()
                            .message(conversation, message_id)
                            .ok()
                            .flatten()
                            .map(|m| m.sender.id),
                        protocol::ReportTarget::Conversation(_) => None,
                    };
                    if let Some(id) = person_id {
                        let _ = demo.ctx.relation(&id, Relation::Blocked);
                    }
                }
                let _ = reply.send(Ok(()));
            }
            Command::ReportReasons { reply, .. } => {
                let _ = reply.send(Ok(protocol::ReportReason::ALL.to_vec()));
            }
            Command::SetPresence { presence, status, status_emoji: _, clear_at: _, reply } => {
                let (_, old) = demo.hub.shown(&demo.key(&demo.me.id));
                demo.ctx.presence(&demo.me.id, presence, status.as_deref().unwrap_or(&old), true);
                let _ = reply.send(Ok(()));
            }
            Command::SetActivity { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetProfile { about, reply, .. } => {
                if let Some(about) = about {
                    let (presence, _) = demo.hub.shown(&demo.key(&demo.me.id));
                    demo.ctx.presence(&demo.me.id, presence, &about, true);
                }
                let _ = reply.send(Ok(()));
            }
            Command::SetAvatar { path: _, reply } => {
                let _ = reply.send(Ok(()));
            }
            Command::SetNickname { space_id: _, nickname: _, reply } => {
                let _ = reply.send(Ok(()));
            }
            Command::Profile { person_id, reply } => drop(reply.send(Ok(demo.about(&person_id, None)))),
            Command::ProfileIn { person_id, space_id, reply } => drop(reply.send(Ok(demo.about(&person_id, Some(&space_id))))),
            Command::ConversationInfo { conversation_id, reply } => drop(reply.send(demo.conversation_about(&conversation_id))),
            Command::SpaceInfo { space_id, reply } => drop(reply.send(demo.space_about(&space_id))),
            Command::Moderate { reply, .. } | Command::Unban { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::SetMute { conversation_id, notify, until: _, reply } => {
                let muted = demo.ctx.muted(&conversation_id, notify == Notify::Nothing);
                let _ = reply.send(muted.map_err(|err| Refusal::Refused(err.to_string())));
            }
            // The demo has no Slack to tell. The flag is still Slack's for a Slack row: it is
            // stored here, which is all the demo's network is.
            Command::Star { conversation_id, starred, reply } => {
                let set = demo.ctx.starred(&conversation_id, starred);
                let _ = reply.send(set.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Leave { conversation_id, silent: _, reply } => {
                let left = demo.ctx.conversation_gone(&conversation_id);
                let _ = reply.send(left.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::CloseDirect { conversation_id, reply } => {
                let closed = demo.ctx.filing(&conversation_id, &Filing { hidden: true, ..Filing::default() });
                let _ = reply.send(closed.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::ClearHistory { conversation_id, delete_chat, reply, .. } => {
                let done = if delete_chat {
                    demo.ctx.conversation_gone(&conversation_id)
                } else {
                    demo.ctx.clear_messages(&conversation_id)
                };
                let _ = reply.send(done.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Pinned { conversation_id, reply } => drop(reply.send(Ok(demo.pinned(&conversation_id)))),
            Command::Pin { conversation_id, message_id, pinned, reply, .. } => {
                let done = demo.ctx.pinned(&conversation_id, &message_id, pinned);
                let _ = reply.send(done.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::UnpinAll { conversation_id, reply } => {
                let done = demo.ctx.unpin_all(&conversation_id);
                let _ = reply.send(done.map_err(|err| Refusal::Refused(err.to_string())));
            }
            Command::Forward { from_conversation_id, message_ids, to_conversation_ids, note, reply } => {
                let from_key = demo.key(&from_conversation_id);
                let (from_title, from_space) = demo
                    .hub
                    .store()
                    .conversation(&from_key)
                    .ok()
                    .flatten()
                    .map(|c| (c.title, c.space_title))
                    .unwrap_or_default();
                let originals: Vec<Message> = message_ids
                    .iter()
                    .filter_map(|id| demo.hub.store().message(&from_key, id).ok().flatten())
                    .collect();
                let mut results = Vec::with_capacity(to_conversation_ids.len());
                let note = note.trim();
                for target in to_conversation_ids {
                    let mut err: Option<String> = None;
                    for orig in &originals {
                        let mut fwd = demo.accepted(&target, orig.text.clone(), None, orig.attachments.clone());
                        fwd.spans = orig.spans.clone();
                        fwd.forwarded = Some(Box::new(protocol::Forwarded {
                            from_title: from_title.clone(),
                            from_space: from_space.clone(),
                            link: String::new(),
                            ..protocol::Forwarded::default()
                        }));
                        if let Err(e) = demo.ctx.message(&target, &fwd) {
                            err = Some(e.to_string());
                            break;
                        }
                    }
                    if err.is_none() && !note.is_empty() {
                        let note_msg = demo.accepted(&target, note.to_owned(), None, Vec::new());
                        if let Err(e) = demo.ctx.message(&target, &note_msg) {
                            err = Some(e.to_string());
                        }
                    }
                    results.push((target, err.map_or(Ok(()), Err)));
                }
                let _ = reply.send(Ok(results));
            }
            Command::SearchShared { conversation_id, kind, before, limit, reply } => {
                let _ = reply.send(demo.search_shared(&conversation_id, kind, before.as_deref(), limit));
            }
            Command::Search { query, scope, filters, reply, .. } => {
                let (conv, space) = match &scope {
                    crate::adapters::SearchScope::Conversation(id) => (Some(id.as_str()), None),
                    crate::adapters::SearchScope::Space(id) => (None, Some(id.as_str())),
                    crate::adapters::SearchScope::Account | crate::adapters::SearchScope::Home => (None, None),
                };
                let found = demo
                    .hub
                    .store()
                    .search_account_filtered(&demo.ctx.account, conv, space, &query, &filters, 100)
                    .map(crate::adapters::SearchPage::from)
                    .map(|mut hits| {
                        if matches!(scope, crate::adapters::SearchScope::Home) {
                            hits.hits.retain(|hit| hit.space_title.is_empty());
                        }
                        hits
                    })
                    .map(Into::into)
                    .map_err(|err| Refusal::Refused(err.to_string()));
                let _ = reply.send(found);
            }
            Command::SearchFiles { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Catalog { kind, query, cursor, reply, .. } => drop(reply.send(Ok(demo.catalog(kind, &query, cursor.as_deref())))),
            Command::ReactionRules { conversation_id, reply } => drop(reply.send(Ok(demo.reaction_rules(&conversation_id)))),
            Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
                Demo::send_with(demo, conversation_id, client_id, text, reply_to, *with);
            }
            Command::CancelSend { client_id, .. } => {
                lock(&demo.cancelled).insert(client_id);
            }
            Command::Previews(ids) => demo.previews(&ids),
            Command::Members { conversation_id, offset, limit, reply } => drop(reply.send(demo.members(&conversation_id, offset, limit))),
            Command::StartDirect { person_id, reply } | Command::StartDirectCall { person_id, reply } => drop(reply.send(demo.start_direct(&person_id))),
            Command::StartGroup { people, title, reply } => {
                let outcome = (|| -> Result<String, Refusal> {
                    let id = format!("group-{}", demo.made.fetch_add(1, Ordering::Relaxed) + 1);
                    let names: Vec<String> = people
                        .iter()
                        .map(|pid| {
                            demo.hub
                                .store()
                                .person(&demo.key(pid))
                                .ok()
                                .flatten()
                                .map(|p| p.name)
                                .filter(|n| !n.is_empty())
                                .unwrap_or_else(|| pid.clone())
                        })
                        .collect();
                    let label = title.unwrap_or_else(|| names.join(", "));
                    let row = ConversationRow::new(id.clone(), ConversationKind::Group, label);
                    demo.ctx.conversation(&row).map_err(|err| Refusal::Refused(err.to_string()))?;
                    Ok(id)
                })();
                let _ = reply.send(outcome);
            }
            Command::CreateChannel { space_id, name, kind, private, group, reply, .. } => {
                let outcome = (|| -> Result<String, Refusal> {
                    let id = format!("chan-{}", demo.made.fetch_add(1, Ordering::Relaxed) + 1);
                    let row = ConversationRow {
                        space: Some(space_id),
                        group,
                        private,
                        ..ConversationRow::new(id.clone(), kind, name)
                    };
                    demo.ctx.conversation(&row).map_err(|err| Refusal::Refused(err.to_string()))?;
                    Ok(id)
                })();
                let _ = reply.send(outcome);
            }
            Command::StartThread { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SearchPeople(query) => demo.ctx.searched(&query),
            Command::ResolveUsername { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Download { url, reply, .. } => drop(reply.send(Ok(Download { url, headers: Vec::new(), sealed: None }))),
            Command::Handoff { conversation_id, reply } => {
                let account = &demo.ctx.account.account;
                let link = match demo.ctx.account.network {
                    Network::Slack => format!("https://app.slack.com/huddle/{account}/{conversation_id}"),
                    Network::Messages => "https://messages.google.com/web/".to_owned(),
                    Network::Tiny => String::new(),
                    _ => format!("https://discord.com/channels/@me/{conversation_id}"),
                };
                let _ = reply.send(Ok(link));
            }
            Command::DiscordVoiceSettings { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::PollVoters { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CallJoin(conversation_id) => Demo::call_join(demo, &conversation_id),
            Command::CallLeave(conversation_id) => demo.call_leave(&conversation_id),
            Command::CallPicture(_) => {}
            Command::CallAudio { muted, deafened } => {
                let call = demo.call.lock().unwrap().clone();
                if let Some(conversation_id) = call.filter(|id| demo.hub.occupants(&demo.key(id)).is_some()) {
                    demo.occupy(&conversation_id, |people, me| people.push(Occupant { muted: muted || deafened, deafened, ..me }));
                }
            }
            // The demo has no Tiny server.
            Command::Tiny(ask) => ask.unsupported(),
            Command::SignOut(reply) => {
                demo.ctx.pause(millis("TINYCLIENT_DEMO_SIGN_OUT_MS", 150));
                let _ = reply.send(Ok(()));
            }
            // The demo's topics are what its store holds: nothing to fetch, and the landing
            // reads it from there.
            Command::OpenForum { reply, .. } => {
                let _ = reply.send(Ok(None));
            }
            Command::AnswerMessageRequest { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::Source { reply, .. } | Command::CreateGroup { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::OpenLink { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::TranslateMessages { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SendRules { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StarMessage { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::SetDisappearing { conversation_id, seconds, reply } => {
                let outcome = demo
                    .ctx
                    .disappearing(&conversation_id, (seconds > 0).then_some(seconds))
                    .map_err(|err| Refusal::Refused(err.to_string()));
                if outcome.is_ok() {
                    let line = if seconds == 0 {
                        "Disappearing messages are off".to_owned()
                    } else {
                        format!("Disappearing messages are set to {}", protocol::format_disappearing(seconds))
                    };
                    let now = crate::hub::now();
                    let mut msg = Message::plain(format!("demo-ttl-{now}"), String::new(), line, now, false);
                    msg.kind = protocol::MessageKind::System;
                    let _ = demo.ctx.message(&conversation_id, &msg);
                }
                let _ = reply.send(outcome);
            }
            Command::MembersAdd { reply, .. }
            | Command::MembersRemove { reply, .. }
            | Command::ConversationPicture { reply, .. }
            | Command::ModerateMember { reply, .. }
            | Command::SetAdmin { reply, .. }
            | Command::TransferOwnership { reply, .. }
            | Command::UpdateChatSettings { reply, .. }
            | Command::SetDefaultSendAs { reply, .. } => {
                let _ = reply.send(Ok(()));
            }
            Command::AdminLog { reply, .. } => {
                let entries: Vec<protocol::AdminLogEntry> = Vec::new();
                let _ = reply.send(Ok(entries));
            }
            Command::SendAsOptions { reply, .. } => {
                let peers: Vec<protocol::SendAs> = Vec::new();
                let _ = reply.send(Ok((peers, None)));
            }
            Command::ConversationUpdate { conversation_id, title, topic, description, reply } => {
                let outcome = match demo.ctx.known(&conversation_id) {
                    Ok(Some(row)) => {
                        let mut updated = crate::store::ConversationRow::new(
                            conversation_id.clone(),
                            row.kind,
                            title.unwrap_or(row.title),
                        );
                        updated.space = row.space;
                        updated.group = row.group;
                        updated.position = row.position;
                        updated.peer = row.peer;
                        updated.call = row.call;
                        updated.topic = topic.or(description).unwrap_or(row.topic);
                        updated.read_only = row.read_only;
                        updated.private = row.private;
                        updated.rights = row.rights;
                        updated.disappearing = row.disappearing;
                        demo.ctx.conversation(&updated).map_err(|err| Refusal::Refused(err.to_string()))
                    }
                    _ => Err(Refusal::Refused("no such conversation".to_owned())),
                };
                let _ = reply.send(outcome);
            }
            Command::File { conversation_id, pinned, archived, folder_add, folder_remove, reply } => {
                let outcome = match demo.ctx.known(&conversation_id) {
                    Ok(Some(row)) => {
                        let mut folders = row.folders;
                        if let Some(f) = folder_add {
                            if !folders.contains(&f) {
                                folders.push(f);
                            }
                        }
                        if let Some(f) = folder_remove {
                            folders.retain(|x| x != &f);
                        }
                        let filing = crate::store::Filing {
                            folders,
                            archived: archived.unwrap_or(row.archived),
                            pin: match pinned {
                                Some(true) => Some(0),
                                Some(false) => None,
                                None => row.pin,
                            },
                            hidden: row.hidden,
                            followed: row.followed,
                        };
                        demo.ctx.filing(&conversation_id, &filing).map_err(|err| Refusal::Refused(err.to_string()))
                    }
                    _ => Err(Refusal::Refused("no such conversation".to_owned())),
                };
                let _ = reply.send(outcome);
            }
            // Nothing is behind these: what is stored is all there is.
            Command::ActivityFeed { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ActivityMarkRead { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Mentions { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::DismissMention { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MoveSpace { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetFolder { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ReadBy { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::BookmarkAdd { reply, .. }
            | Command::BookmarkEdit { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::BookmarkRemove { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::BrowseChannels { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SaveGif { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StickerSet { reply, .. } | Command::FaveSticker { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StarsBalance { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SendPaidReaction { reply, .. }
            | Command::TogglePaidReactionPrivacy { reply, .. }
            | Command::SetDefaultReaction { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Favourite { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Transcribe { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::RateTranscript { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Translate { conversation_id, message_id, to, reply } => {
                let original = demo
                    .ctx
                    .stored_message(&conversation_id, &message_id)
                    .ok()
                    .flatten()
                    .map(|m| m.text)
                    .unwrap_or_default();
                let target_lang = protocol::normalize_language_tag(&to);
                let outcome = if original.trim().is_empty() {
                    Err(Refusal::Refused("Nothing to translate.".to_owned()))
                } else {
                    Ok(protocol::Translation {
                        text: if original == super::telegram::FERRY_ORIGINAL && target_lang == "en" {
                            super::telegram::FERRY_ENGLISH.to_owned()
                        } else {
                            format!("[{target_lang}] {original}")
                        },
                        source_lang: (original == super::telegram::FERRY_ORIGINAL).then(|| "es".to_owned()),
                        target_lang,
                    })
                };
                let _ = reply.send(outcome);
            }
            Command::PreviewChannel { reply, .. }
            | Command::Join { reply, .. }
            | Command::DeleteFile { reply, .. }
            | Command::InviteMember { reply, .. }
            | Command::AddCustomEmoji { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MessageAction { conversation_id, message_id, action_id, value, values, reply, .. } => {
                let outcome = (|| -> Result<(), Refusal> {
                    let stored = demo.ctx.stored_message(&conversation_id, &message_id).ok().flatten();
                    let mut message = stored.ok_or_else(|| Refusal::Refused("That message is gone.".to_owned()))?;
                    let picked = if !values.is_empty() {
                        values.join(", ")
                    } else if !value.is_empty() {
                        value.clone()
                    } else {
                        action_id.clone()
                    };
                    for ctrl in &mut message.controls {
                        if ctrl.action_id == action_id {
                            match &mut ctrl.kind {
                                protocol::ControlKind::Select { selected, .. }
                                | protocol::ControlKind::RadioButtons { selected, .. } => {
                                    *selected = Some(picked.clone());
                                }
                                protocol::ControlKind::Checkboxes { selected, .. } => {
                                    *selected = values.clone();
                                }
                                protocol::ControlKind::Button { .. } => {
                                    if !ctrl.label.ends_with(" ✓") {
                                        ctrl.label.push_str(" ✓");
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    if action_id == "demo_open_modal" {
                        demo.ctx.interaction_modal(
                            &conversation_id,
                            protocol::InteractionModal {
                                id: format!("modal-{message_id}"),
                                application_id: "demo-bot".into(),
                                custom_id: "demo_feedback".into(),
                                title: "Submit note".into(),
                                fields: vec![protocol::ModalField {
                                    custom_id: "note".into(),
                                    label: "Your note".into(),
                                    value: String::new(),
                                    placeholder: "Type something…".into(),
                                    required: true,
                                    multiline: false,
                                    min_length: Some(1),
                                    max_length: Some(200),
                                }],
                                nonce: String::new(),
                            },
                        );
                    }
                    demo.ctx.message(&conversation_id, &message).map_err(|err| Refusal::Refused(err.to_string()))
                })();
                let _ = reply.send(outcome.map(|()| None));
            }
            Command::Commands { query, kind, reply, .. } => {
                let mut cmds = vec![
                    protocol::AppCommand {
                        id: "demo-shrug".into(),
                        application_id: "builtin".into(),
                        version: "1".into(),
                        kind: protocol::CommandKind::ChatInput,
                        name: "shrug".into(),
                        description: "Append ¯\\_(ツ)_/¯ to your message".into(),
                        app_name: "Built-in".into(),
                        options: vec![protocol::CommandOption {
                            name: "message".into(),
                            description: "Text before the shrug".into(),
                            kind: protocol::CommandOptionKind::String,
                            required: false,
                            ..Default::default()
                        }],
                        built_in: true,
                    },
                    protocol::AppCommand {
                        id: "demo-deploy".into(),
                        application_id: "demo-bot".into(),
                        version: "1".into(),
                        kind: protocol::CommandKind::ChatInput,
                        name: "deploy".into(),
                        description: "Post a demo deployment approval with buttons".into(),
                        app_name: "DeployBot".into(),
                        options: vec![protocol::CommandOption {
                            name: "env".into(),
                            description: "Target environment".into(),
                            kind: protocol::CommandOptionKind::String,
                            required: true,
                            autocomplete: true,
                            choices: vec![
                                protocol::CommandChoice { name: "Staging".into(), value: "staging".into() },
                                protocol::CommandChoice { name: "Production".into(), value: "production".into() },
                            ],
                            ..Default::default()
                        }],
                        built_in: false,
                    },
                ];
                if let Some(k) = kind {
                    cmds.retain(|c| c.kind == k);
                }
                let q = query.trim().to_ascii_lowercase();
                if !q.is_empty() {
                    cmds.retain(|c| c.name.to_ascii_lowercase().contains(&q) || c.description.to_ascii_lowercase().contains(&q));
                }
                let _ = reply.send(Ok(cmds));
            }
            Command::CommandAutocomplete { focused, options, reply, .. } => {
                let typed = options
                    .iter()
                    .find(|a| a.name == focused)
                    .map(|a| a.value.to_ascii_lowercase())
                    .unwrap_or_default();
                let all = [
                    ("Staging", "staging"),
                    ("Canary", "canary"),
                    ("Production", "production"),
                ];
                let choices = all
                    .into_iter()
                    .filter(|(name, val)| typed.is_empty() || name.to_ascii_lowercase().contains(&typed) || val.contains(&typed))
                    .map(|(name, val)| protocol::CommandChoice { name: name.into(), value: val.into() })
                    .collect();
                let _ = reply.send(Ok(choices));
            }
            Command::RunCommand { conversation_id, name, arguments, reply, .. } => {
                let now = crate::hub::now();
                let outcome = match name.as_str() {
                    "shrug" => {
                        let text = arguments.first().map(|a| a.value.trim()).unwrap_or("");
                        let body = if text.is_empty() {
                            "¯\\_(ツ)_/¯".to_owned()
                        } else {
                            format!("{text} ¯\\_(ツ)_/¯")
                        };
                        let msg = demo.accepted(&conversation_id, body, None, Vec::new());
                        demo.ctx.message(&conversation_id, &msg).map_err(|err| Refusal::Refused(err.to_string()))
                    }
                    "deploy" => {
                        let env = arguments
                            .iter()
                            .find(|a| a.name == "env")
                            .or_else(|| arguments.first())
                            .map(|a| a.value.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or("staging");
                        let id = format!("demo-cmd-{}", demo.made.fetch_add(1, Ordering::Relaxed) + 1);
                        let mut msg = Message::plain(id, "DeployBot", format!("Deploy to `{env}`?"), now, false);
                        msg.sender.bot = true;
                        msg.controls = vec![
                            protocol::MessageControl {
                                block_id: "demo-deploy".into(),
                                action_id: "approve".into(),
                                element_type: "button".into(),
                                label: "Approve".into(),
                                value: "approved".into(),
                                confirm: None,
                                kind: protocol::ControlKind::Button { url: String::new(), style: protocol::ControlStyle::Success },
                                disabled: false,
                                emoji: String::new(),
                                row: 0,
                                min_values: 0,
                                max_values: 0,
                            },
                            protocol::MessageControl {
                                block_id: "demo-deploy".into(),
                                action_id: "reject".into(),
                                element_type: "button".into(),
                                label: "Cancel".into(),
                                value: "cancelled".into(),
                                confirm: None,
                                kind: protocol::ControlKind::Button { url: String::new(), style: protocol::ControlStyle::Danger },
                                disabled: false,
                                emoji: String::new(),
                                row: 0,
                                min_values: 0,
                                max_values: 0,
                            },
                        ];
                        demo.ctx.message(&conversation_id, &msg).map_err(|err| Refusal::Refused(err.to_string()))
                    }
                    _ => Err(Refusal::Refused(format!("Unknown command /{name}."))),
                };
                let _ = reply.send(outcome);
            }
            Command::ModalSubmit { conversation_id, values, reply, .. } => {
                let summary = values
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let msg = demo.accepted(&conversation_id, format!("Submitted form ({summary})"), None, Vec::new());
                let res = demo.ctx.message(&conversation_id, &msg).map_err(|err| Refusal::Refused(err.to_string()));
                let _ = reply.send(res);
            }
            Command::UserGroups { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::UserGroupCreate { reply, .. }
            | Command::UserGroupUpdate { reply, .. }
            | Command::UserGroupMembers { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::AppView { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SectionCreate { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SectionRename { reply, .. }
            | Command::SectionDelete { reply, .. }
            | Command::SectionReorder { reply, .. }
            | Command::SectionMoveChannel { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SlashCommands { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::AllUnreads { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::AccountFiles { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CallRing { reply, .. }
            | Command::CallStopRinging { reply, .. }
            | Command::CallRaiseHand { reply, .. }
            | Command::CallReact { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)));
            }
            Command::CallAddPeople { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)));
            }
            Command::CallHistory { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Voicemails { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::VoicemailAudio { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ChangeVoicemail { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::DeleteCallEntries { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::GetPrivacy { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetPrivacy { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::GetOutOfOffice { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetOutOfOffice { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SpaceTags { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StarredMessages { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)));
            }
            Command::Remind { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)));
            }
            Command::Reactors { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ClearReactions { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::AddFriend { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetNote { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetFriendNickname { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::InvitePreview { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::InviteJoin { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateInviteLink { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::TransferOwner { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateSpace { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Events { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::EventInterest { reply, .. }
            | Command::EditSpace { reply, .. }
            | Command::DeleteSpace { reply, .. }
            | Command::TransferSpace { reply, .. }
            | Command::DeleteEvent { reply, .. }
            | Command::RevokeSpaceInvite { reply, .. }
            | Command::SetInvitesPaused { reply, .. }
            | Command::MoveRoles { reply, .. }
            | Command::DeleteRole { reply, .. }
            | Command::SetMemberRoles { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SpaceInvites { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Roles { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateRole { reply, .. } | Command::EditRole { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateEvent { reply, .. } | Command::EditEvent { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::EventPreview { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::EventLinkInterest { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Discover { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::JoinDiscovered { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SpaceGate { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::AcceptRules { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SubmitOnboarding { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ChatInvites { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateInvite { reply, .. } | Command::RevokeInvite { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::JoinRequests { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::HandleJoinRequest { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::LeaveSpace { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetSpaceNotify { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SpaceIntegrations { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateWebhook { reply, .. } | Command::EditWebhook { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::DeleteWebhook { reply, .. } | Command::RemoveIntegration { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::SpaceExpressions { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateEmoji { reply, .. } | Command::EditEmoji { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::CreateSticker { reply, .. } | Command::EditSticker { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::DeleteEmoji { reply, .. } | Command::DeleteSticker { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::GetDefaultDisappearing { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetDefaultDisappearing { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::FolderSave { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::FolderDelete { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::FoldersReorder { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SuggestedFolders { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::LoadTopics { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::TopicCreate { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::TopicUpdate { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::TopicDelete { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::TopicIcons { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MessageEffects { reply } => {
                let items = [("confetti", "\u{1F389}"), ("fire", "\u{1F525}"), ("hearts", "\u{2764}\u{FE0F}")]
                    .into_iter()
                    .map(|(id, emoji)| protocol::CatalogItem {
                        id: id.to_owned(),
                        name: emoji.to_owned(),
                        file: None,
                        pack: String::new(),
                        animated: false,
                        set: String::new(),
                        locked: false,
                    })
                    .collect();
                let _ = reply.send(Ok(crate::adapters::CatalogPage { items, cursor: None }));
            }
            Command::CallRate { reply, .. } => {
                let _ = reply.send(Ok(()));
            }
            Command::ScheduledList { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CancelScheduled { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SendScheduledNow { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::JumpToDate { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SearchCalendar { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::DeleteMany { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::DeleteParticipantHistory { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ReportSpamMessages { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Reschedule { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::FollowThread { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ForumInfo { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ForumPost { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::PressButton { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::GetNotifyDefaults { reply }
            | Command::SetNotifyDefault { reply, .. }
            | Command::ResetNotifyDefaults { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SavedDialogs { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SavedHistory { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ActiveSessions { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::EndSession { reply, .. }
            | Command::EndOtherDiscordSessions { reply }
            | Command::RemoveDiscordAccountEntry { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::DiscordAccountEntries { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::UnreadItems { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MarkUnreadItemsRead { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ChannelThreads { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ThreadUpdate { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::DeleteThread { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Publish { reply, .. } | Command::Follow { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::FollowChannel { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetSpaceView { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetSpaceOptions { reply, .. } | Command::SetSpacePrivacy { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::GetNetworkPrivacy { reply } | Command::SetNetworkPrivacy { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateCategory { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetVoiceChannelStatus { reply, .. }
            | Command::EditDiscordVoiceChannel { reply, .. }
            | Command::SetDiscordCallRegion { reply, .. }
            | Command::CallModerate { reply, .. }
            | Command::CallMove { reply, .. }
            | Command::EditChannel { reply, .. }
            | Command::MoveChannels { reply, .. }
            | Command::DeleteChannel { reply, .. }
            | Command::SetOverwrite { reply, .. }
            | Command::DeleteOverwrite { reply, .. }
            | Command::SyncChannelPermissions { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ChannelPermissions { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::AuditLog { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::AutomodRules { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SaveAutomodRule { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::DeleteAutomodRule { reply, .. } | Command::SaveSpaceSafety { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::GetAutoDownload { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetAutoDownload { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::GetBoosts { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ApplyBoost { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::GetBotMenu { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StartBot { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SharePhone { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Meetings { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MeetingTranscript { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MeetingInvitees { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateMeeting { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MeetingRsvp { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Stories { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ReadStory { reply, .. }
            | Command::ReactStory { reply, .. }
            | Command::StopLiveLocation { reply, .. }
            | Command::StopAllLiveLocations { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ReplyStory { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SendLocation { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StartLiveLocation { reply, .. }
            | Command::UpdateLiveLocation { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ActiveLiveLocations { reply } => drop(reply.send(Ok(Vec::new()))),
            Command::Backfill(_)
            | Command::Warm(_)
            | Command::History { .. }
            | Command::HistoryAfter { .. }
            | Command::HistoryAround { .. }
            | Command::OpenSpace(_)
            | Command::OpenHome
            | Command::OpenThreads(_)
            | Command::MarkRead { .. }
            | Command::MarkUnread { .. }
            | Command::Viewed { .. }
            | Command::MarkPlayed { .. }
            | Command::AckBulk { .. }
            | Command::Collapse { .. }
            | Command::Typing { .. }
            | Command::FollowLink { .. }
            | Command::Draft { .. }
            | Command::Watch(_)
            | Command::Wanted(_)
            | Command::WantedSpan { .. }
            | Command::TelegramCallSignal { .. }
            | Command::TelegramCallSpeaking { .. }
            | Command::GroupCallParticipantGain { .. }
            | Command::CallJoinVideo(_)
            | Command::Played { .. }
            | Command::CallIce { .. }
            | Command::Stop => {}
            Command::GroupCallSchedule { reply, .. }
            | Command::GroupCallStart { reply, .. }
            | Command::GroupCallSubscribe { reply, .. }
            | Command::GroupCallJoin { reply, .. }
            | Command::GroupCallSaveDefaultJoinAs { reply, .. }
            | Command::GroupCallEditTitle { reply, .. }
            | Command::GroupCallDiscard { reply, .. }
            | Command::GroupCallSettings { reply, .. }
            | Command::GroupCallRaiseHand { reply, .. }
            | Command::GroupCallModerate { reply, .. }
            | Command::GroupCallRtmpQuality { reply, .. }
            | Command::ConferenceJoin { reply, .. }
            | Command::ConferenceRemove { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::GroupCallRecord { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::GroupCallRtmpUrl { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::GroupCallJoinAs { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::GroupCallCreate { reply, .. }
            | Command::GroupCallExportInvite { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::GroupCallInvite { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
            Command::ConferenceResolve { reply, .. } | Command::ConferenceCreate { reply, .. } => {
                let _ = reply.send(Err(Refusal::Unsupported));
            }
        }
    }
}
