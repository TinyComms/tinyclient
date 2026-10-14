//! What answers for a demo account: an adapter with no network behind it. It serves the commands
//! the shell's requests become, with the same `Context` calls a real adapter makes, so the shell
//! can be clicked through and the server's own tests have a network that does as it is told.
//!
//! - `TINYCHAT_DEMO_VOICE` (`host:port`): a voice server to hand mediad when a call starts.
//!   Without it a call gets no voice session, as on a network that never answers.
//! - `TINYCHAT_DEMO_ANSWER_MS`: how long the other side of a direct call takes to pick up
//!   (4000). Only with a voice server; without one nobody answers.
//! - `TINYCHAT_DEMO_SIGN_OUT_MS`: how long ending the session on the "network" takes (150).

use super::Who;
use crate::adapters::{self, About, Adapter, Command, Context, Download, Members, Refusal};
use crate::hub::{now, Hub};
use crate::store::ConversationRow;
use protocol::{
    AccountKey, CallSupport, ConversationKind, Dave, Key, MemberSection, Message, Network,
    Occupant, Participant, Presence, Reaction, RelateAction, Relation, ReplyTo, VoiceJoin,
};
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
            _ => format!("500000000000000{number}"),
        }
    }

    fn send(&self, conversation_id: &str, client_id: &str, text: String, reply_to: Option<String>) {
        let sent = now();
        let mut message = Message::plain(self.message_id(sent), "", text, sent, true);
        message.sender = self.me.sender();
        let quoted = reply_to.and_then(|id| self.ctx.stored_message(conversation_id, &id).ok().flatten());
        message.reply_to = quoted.map(|quoted| ReplyTo {
            id: quoted.id,
            person: (!quoted.sender.id.is_empty()).then(|| self.key(&quoted.sender.id)),
            sender: quoted.sender.name,
            snippet: quoted.text.chars().take(120).collect(),
            deleted: quoted.deleted,
        });
        self.ctx.sent(conversation_id, client_id, Ok(message));
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

    fn about(&self, person_id: &str) -> About {
        let pick = person_id.bytes().fold(0usize, |sum, byte| sum.wrapping_mul(31).wrapping_add(byte as usize));
        let spaces = self.hub.store().spaces(Some(self.ctx.account.network)).unwrap_or_default();
        let ours = spaces.into_iter().filter(|space| space.key.account() == self.ctx.account);
        let (zone, offset) = ZONES[pick % ZONES.len()];
        About {
            bio: BIOS[pick % BIOS.len()].to_owned(),
            mutual_spaces: ours.map(|space| space.name).take(1 + pick % 4).collect(),
            time_zone: zone.to_owned(),
            utc_offset: Some(offset),
            fields: match self.ctx.account.network {
                Network::Slack => vec![("Title".to_owned(), "Platform engineer".to_owned()), ("Email".to_owned(), format!("{}@harbor.example", person_id.to_lowercase()))],
                _ => vec![("Username".to_owned(), format!("user{}", pick % 1000))],
            },
            since: Some(1_500_000_000 + (pick % 200_000_000) as i64),
            mutual_friends: (pick % 9) as u32,
            accent: Some([0xd9_82_4a, 0x4c_9a_6a, 0x5b_6e_e1, 0xb5_4a_7a][pick % 4]),
            banner: None,
            nickname: String::new(),
            pronouns: String::new(),
            title: String::new(),
            department: String::new(),
            roles: Vec::new(),
            contacts: Vec::new(),
            mutual_total: 0,
            joined: None,
            friends_since: None,
            last_seen: None,
            status_until: None,
            away: String::new(),
            away_until: None,
            note: String::new(),
            warning: None,
        }
    }

    /// The people the account owns, as a member list: who is around, then who is not.
    fn members(&self, offset: u32, limit: u32) -> Result<Members, Refusal> {
        let owned = self.hub.store().people(Some(self.ctx.account.network), None, None, 0, protocol::PAGE_LIMIT);
        let (mut people, _) = owned.map_err(|err| Refusal::Refused(err.to_string()))?;
        people.retain(|person| person.key.account() == self.ctx.account);
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
            Command::Profile { person_id, reply } => drop(reply.send(Ok(demo.about(&person_id)))),
            Command::Members { offset, limit, reply, .. } => drop(reply.send(demo.members(offset, limit))),
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
            | Command::MarkRead { .. }
            | Command::Typing(_)
            | Command::Watch(_)
            | Command::Stop => {}
            Command::SendWith { conversation_id, client_id, text, reply_to, .. } => {
                demo.send(&conversation_id, &client_id, text, reply_to);
            }
            other => other.unsupported(&demo.ctx),
        }
    }
}
