//! The wire to Signal itself: presage, on one current-thread tokio runtime and its `LocalSet`, on a
//! thread of the wire's own. The adapter's threads ask it things over a channel and wait for the
//! answer; what Signal says unasked becomes `Event`s.
//!
//! Built only with the `signal` feature. Untested against a live account: no test can reach
//! Signal without a real phone to link from (see `tests/signal.rs` for what is proven).

use super::vault::{self, Vault};
use super::wire::{Body, Contact, Event, Fault, GroupInfo, Linked, Linker, Peer, Quote, Text, Wire};
use super::Saved;
use futures::StreamExt;
use presage::libsignal_service::content::ContentBody;
use presage::libsignal_service::prelude::{Content, GroupMasterKey, GroupSecretParams, PushService, ServiceCredentials, ServiceError, Uuid};
use presage::libsignal_service::protocol::{Aci, ServiceId};
use presage::libsignal_service::sender::MessageSenderError;
use presage::libsignal_service::utils::TryIntoE164;
use presage::libsignal_service::websocket::Identified;
use presage::libsignal_service::configuration::SignalServers;
use presage::manager::{Registered, RegistrationData};
use presage::model::messages::Received;
use presage::proto::{
    body_range::AssociatedValue, data_message, receipt_message, sync_message, typing_message, DataMessage, EditMessage, GroupContextV2,
    ReceiptMessage, SyncMessage, TypingMessage,
};
use presage::store::ContentsStore;
use presage::Manager;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc as async_mpsc;

/// How long the adapter waits for one answer from the wire's thread.
const ANSWER: Duration = Duration::from_secs(45);
/// How long connecting may take before the attempt counts as dropped.
const CONNECT: Duration = Duration::from_secs(60);

type Signal = Manager<Vault, Registered>;

/// What the adapter's threads ask of the wire's thread.
enum Ask {
    Send { to: Peer, body: Body, answer: Sender<Result<u64, Fault>> },
    Typing { to: Peer, started: bool },
    Read { read: Vec<(String, u64)>, receipts: bool, answer: Sender<Result<(), Fault>> },
    Unlink { answer: Sender<Result<(), Fault>> },
}

pub struct Real {
    me: (String, String),
    asks: async_mpsc::UnboundedSender<Ask>,
    heard: Mutex<Option<Receiver<Event>>>,
    closed: Arc<AtomicBool>,
    stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    text.len().is_multiple_of(2).then_some(())?;
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok()).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A group's public id: what the contract and typing messages name it by. Its master key opens
/// the group and stays in the vault.
fn group_id(master_key: &[u8; 32]) -> String {
    hex(&GroupSecretParams::derive_from_master_key(GroupMasterKey::new(*master_key)).get_group_identifier())
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|err| err.to_string())
}

fn open_vault(saved: &Saved) -> Result<Vault, String> {
    let key = unhex(&saved.vault).filter(|key| key.len() == 32).ok_or("the Signal session has no key to its state")?;
    let path = vault::path_of(&saved.aci);
    if !path.exists() {
        return Err("this computer's Signal state is gone; sign in to Signal again".to_owned());
    }
    Vault::open(&path, &key)
}

/// What this device kept of an account, gone.
pub fn forget(saved: &Saved) {
    vault::remove(&vault::path_of(&saved.aci));
}

/// A refusal in words, and whether it means this device is no longer linked.
fn fault<E: std::error::Error>(err: presage::Error<E>) -> Fault {
    use presage::Error as P;
    let sender = match &err {
        P::MessageSenderError(sender) => Some(&**sender),
        _ => None,
    };
    match (&err, sender) {
        (P::ServiceError(ServiceError::Unauthorized), _) | (_, Some(MessageSenderError::ServiceError(ServiceError::Unauthorized))) => {
            Fault::Ended("This computer was removed from your Signal account.".to_owned())
        }
        (_, Some(MessageSenderError::ProofRequired { .. })) => Fault::Refused(
            "Signal wants to check this is a person. Send a message from Signal on your phone, then try again.".to_owned(),
        ),
        (_, Some(MessageSenderError::NotFound { .. })) => Fault::Refused("That person is no longer on Signal.".to_owned()),
        (P::ServiceError(ServiceError::RateLimitExceeded { .. }), _) => Fault::Refused("Signal asked for a pause. Try again in a minute.".to_owned()),
        (P::UnknownGroup, _) => Fault::Refused("This group isn't one Signal still knows for you.".to_owned()),
        _ => Fault::Net(err.to_string()),
    }
}

impl Real {
    pub fn open(saved: &Saved) -> Result<Real, String> {
        let vault = open_vault(saved)?;
        let (events, heard) = channel();
        let (asks, mut asked) = async_mpsc::unbounded_channel::<Ask>();
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready, readied) = channel::<Result<(String, String), String>>();
        let closed: Arc<AtomicBool> = Arc::default();
        let thread_closed = closed.clone();
        let fallback = saved.name.clone();
        std::thread::Builder::new()
            .name("signal-wire".into())
            .spawn(move || {
                let runtime = match runtime() {
                    Ok(runtime) => runtime,
                    Err(why) => return drop(ready.send(Err(why))),
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, async move {
                    let mut manager = match Manager::load_registered(vault.clone()).await {
                        Ok(manager) => manager,
                        Err(err) => return drop(ready.send(Err(err.to_string()))),
                    };
                    let mut messages = match manager.receive_messages().await {
                        Ok(messages) => Box::pin(messages),
                        Err(err) => {
                            let ended = matches!(fault(err), Fault::Ended(_));
                            let _ = ready.send(Err(if ended { "ended".to_owned() } else { "Signal could not be reached".to_owned() }));
                            if ended {
                                let _ = events.send(Event::Ended("This computer was removed from your Signal account.".to_owned()));
                            }
                            return;
                        }
                    };
                    let aci = manager.registration_data().service_ids.aci.to_string();
                    let name = own_name(&mut manager).await.unwrap_or(fallback);
                    let mut wire = Session { manager, vault, me: aci.clone(), events, names: HashMap::new(), groups: HashMap::new() };
                    let _ = ready.send(Ok((aci, name)));
                    wire.first_run().await;
                    wire.groups_changed().await;
                    let why = loop {
                        tokio::select! {
                            received = messages.next() => match received {
                                Some(received) => wire.received(received).await,
                                None => break wire.why_closed().await,
                            },
                            Some(ask) = asked.recv() => wire.ask(ask).await,
                            _ = &mut stopped => break Event::Closed("closed".to_owned()),
                        }
                    };
                    let _ = wire.events.send(why);
                });
                thread_closed.store(true, Ordering::Relaxed);
            })
            .map_err(|err| err.to_string())?;
        let (me, name) = match readied.recv_timeout(CONNECT) {
            Ok(Ok(found)) => found,
            Ok(Err(why)) => {
                // An unlinked device says so through its events: hand them over all the same.
                if why == "ended" {
                    return Ok(Real { me: (saved.aci.clone(), saved.name.clone()), asks, heard: Mutex::new(Some(heard)), closed, stop: Mutex::new(Some(stop)) });
                }
                return Err(why);
            }
            Err(_) => return Err("Signal did not answer".to_owned()),
        };
        Ok(Real { me: (me, name), asks, heard: Mutex::new(Some(heard)), closed, stop: Mutex::new(Some(stop)) })
    }

    fn ask<T>(&self, make: impl FnOnce(Sender<Result<T, Fault>>) -> Ask) -> Result<T, Fault> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(Fault::Net("closed".into()));
        }
        let (answer, answered) = channel();
        self.asks.send(make(answer)).map_err(|_| Fault::Net("closed".into()))?;
        answered.recv_timeout(ANSWER).map_err(|_| Fault::Net("no answer".into()))?
    }
}

impl Wire for Real {
    fn events(&self) -> Option<Receiver<Event>> {
        self.heard.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
    }

    fn me(&self) -> (String, String) {
        self.me.clone()
    }

    fn send(&self, to: &Peer, body: Body) -> Result<u64, Fault> {
        self.ask(|answer| Ask::Send { to: to.clone(), body, answer })
    }

    fn typing(&self, to: &Peer, started: bool) -> Result<(), Fault> {
        self.asks.send(Ask::Typing { to: to.clone(), started }).map_err(|_| Fault::Net("closed".into()))
    }

    fn read(&self, read: &[(String, u64)], receipts: bool) -> Result<(), Fault> {
        self.ask(|answer| Ask::Read { read: read.to_vec(), receipts, answer })
    }

    fn unlink(&self) -> Result<(), Fault> {
        self.ask(|answer| Ask::Unlink { answer })
    }

    fn close(&self) {
        if let Some(stop) = self.stop.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            let _ = stop.send(());
        }
    }
}

impl Drop for Real {
    fn drop(&mut self) {
        self.close();
    }
}

async fn own_name(manager: &mut Signal) -> Option<String> {
    let profile = manager.retrieve_profile().await.ok()?;
    profile.name.map(|name| name.to_string()).filter(|name| !name.trim().is_empty())
}

/// One connection's state, on the wire's thread.
struct Session {
    manager: Signal,
    vault: Vault,
    me: String,
    events: Sender<Event>,
    /// Names looked up, so each person is asked for once a connection.
    names: HashMap<String, String>,
    /// Groups by public id: their master keys.
    groups: HashMap<String, [u8; 32]>,
}

impl Session {
    fn tell(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Once per link: ask the phone for the contacts and the settings it shares with devices.
    async fn first_run(&mut self) {
        if self.vault.flag("asked") {
            self.tell(Event::Contacts(self.contacts().await));
            return;
        }
        let _ = self.manager.request_contacts().await;
        let configuration = SyncMessage {
            request: Some(sync_message::Request { r#type: Some(sync_message::request::Type::Configuration.into()) }),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        if let Ok(me) = Uuid::parse_str(&self.me) {
            let _ = self.manager.send_message(Aci::from(me), configuration, now_ms()).await;
        }
        self.vault.set_flag("asked");
    }

    async fn contacts(&self) -> Vec<Contact> {
        let Ok(stored) = self.manager.store().contacts().await else {
            return Vec::new();
        };
        stored
            .filter_map(Result::ok)
            .map(|contact| Contact {
                id: contact.uuid.to_string(),
                name: contact.name.clone(),
                phone: contact.phone_number.as_ref().map(ToString::to_string).unwrap_or_default(),
            })
            .collect()
    }

    /// The groups as stored, told whole, and their keys remembered.
    async fn groups_changed(&mut self) {
        let Ok(groups) = self.vault.group_keys() else {
            return;
        };
        let mut told = Vec::new();
        for (key, group) in groups {
            let id = group_id(&key);
            self.groups.insert(id.clone(), key);
            told.push(GroupInfo { id, title: group.title, description: group.description.unwrap_or_default(), members: group.members.len() as u32 });
        }
        self.tell(Event::Groups(told));
    }

    /// Why the messages ended: an unlinked device is told apart from a dropped connection.
    async fn why_closed(&mut self) -> Event {
        match self.manager.whoami().await {
            Err(err) => match fault(err) {
                Fault::Ended(why) => Event::Ended(why),
                other => Event::Closed(other.to_string()),
            },
            Ok(_) => Event::Closed("the connection to Signal closed".to_owned()),
        }
    }

    async fn name_of(&mut self, aci: &str) -> String {
        if let Some(name) = self.names.get(aci) {
            return name.clone();
        }
        let Ok(uuid) = Uuid::parse_str(aci) else {
            return String::new();
        };
        let id = ServiceId::Aci(Aci::from(uuid));
        let mut name = match self.manager.store().contact_by_id(&id).await {
            Ok(Some(contact)) if !contact.name.is_empty() => contact.name,
            _ => String::new(),
        };
        if name.is_empty() {
            if let Ok(Some(key)) = self.manager.store().profile_key(&id).await {
                if let Ok(profile) = self.manager.retrieve_profile_by_uuid(Aci::from(uuid), key).await {
                    name = profile.name.map(|name| name.to_string()).unwrap_or_default();
                }
            }
        }
        self.names.insert(aci.to_owned(), name.clone());
        name
    }

    /// Where a data message belongs: its group, or the person.
    async fn peer(&mut self, group: Option<&GroupContextV2>, person: &str) -> Peer {
        let key = group.and_then(|group| group.master_key.as_deref()).and_then(|key| <[u8; 32]>::try_from(key).ok());
        match key {
            Some(key) => {
                let id = group_id(&key);
                if !self.groups.contains_key(&id) {
                    self.groups.insert(id.clone(), key);
                    self.groups_changed().await;
                }
                Peer::Group(id)
            }
            None => Peer::Direct(person.to_owned()),
        }
    }

    /// The words of a message: mentions read as names, and what is not drawn here named.
    async fn text(&mut self, message: &DataMessage, author: &str, sent: u64) -> Text {
        let mut text = message.body.clone().unwrap_or_default();
        let mut mentions_me = false;
        let mut mentions: Vec<(u32, String)> = Vec::new();
        for range in &message.body_ranges {
            if let Some(AssociatedValue::MentionAci(aci)) = &range.associated_value {
                mentions_me |= *aci == self.me;
                mentions.push((range.start.unwrap_or(0), aci.clone()));
            }
        }
        if !mentions.is_empty() {
            mentions.sort();
            let mut names = Vec::new();
            for (_, aci) in &mentions {
                let name = if *aci == self.me { "you".to_owned() } else { self.name_of(aci).await };
                names.push(if name.is_empty() { "@Signal user".to_owned() } else { format!("@{name}") });
            }
            // Each mention is one U+FFFC in the body, in order.
            let mut named = names.into_iter();
            text = text.chars().map(|c| if c == '\u{FFFC}' { named.next().unwrap_or_default() } else { c.to_string() }).collect();
        }
        let unsupported = if let Some(attachment) = message.attachments.first() {
            let kind = attachment.content_type.as_deref().unwrap_or("");
            Some(match kind.split('/').next() {
                Some("image") => "Photo",
                Some("video") => "Video",
                Some("audio") => "Audio",
                _ => "File",
            })
        } else if message.sticker.is_some() {
            Some("Sticker")
        } else if !message.contact.is_empty() {
            Some("Contact")
        } else if message.payment.is_some() {
            Some("Payment")
        } else if message.gift_badge.is_some() {
            Some("Gift badge")
        } else if message.is_view_once.unwrap_or(false) {
            Some("View-once media")
        } else {
            None
        };
        let quote = message.quote.as_ref().map(|quote| Quote {
            sent: quote.id.unwrap_or(0),
            author: quote.author_aci.clone().unwrap_or_default(),
            text: quote.text.clone().unwrap_or_default(),
        });
        let name = if author == self.me { String::new() } else { self.name_of(author).await };
        Text { author: author.to_owned(), name, sent, text, quote, mentions_me, unsupported: unsupported.map(str::to_owned) }
    }

    /// One data message, from someone (`outgoing` false) or from us on another device.
    async fn data(&mut self, message: &DataMessage, author: &str, peer: Peer, sent: u64, outgoing: bool) {
        if let Some(reaction) = &message.reaction {
            let target_author = reaction.target_author_aci.clone().unwrap_or_default();
            self.tell(Event::Reacted {
                peer,
                by: author.to_owned(),
                author: target_author,
                target: reaction.target_sent_timestamp.unwrap_or(0),
                emoji: reaction.emoji.clone().unwrap_or_default(),
                removed: reaction.remove.unwrap_or(false),
            });
            return;
        }
        if let Some(delete) = &message.delete {
            self.tell(Event::Deleted { peer, author: author.to_owned(), target: delete.target_sent_timestamp.unwrap_or(0) });
            return;
        }
        // Timer changes, profile key updates and group changes carry nothing to show.
        let empty = message.body.as_deref().unwrap_or("").is_empty();
        let carries = !message.attachments.is_empty() || message.sticker.is_some() || !message.contact.is_empty() || message.payment.is_some() || message.gift_badge.is_some();
        if empty && !carries {
            return;
        }
        let text = self.text(message, author, sent).await;
        self.tell(Event::Message { peer, text, outgoing });
    }

    async fn received(&mut self, received: Received) {
        match received {
            Received::QueueEmpty => self.tell(Event::Caught),
            Received::Contacts => {
                self.names.clear();
                let contacts = self.contacts().await;
                self.tell(Event::Contacts(contacts));
            }
            Received::Content(content) => self.content(*content).await,
        }
    }

    async fn content(&mut self, content: Content) {
        let sender = content.metadata.sender.raw_uuid().to_string();
        let sent = content.metadata.timestamp.timestamp_millis().max(0) as u64;
        match content.body {
            ContentBody::DataMessage(message) => {
                let peer = self.peer(message.group_v2.as_ref(), &sender).await;
                let sent = message.timestamp.unwrap_or(sent);
                self.data(&message, &sender, peer, sent, false).await;
            }
            ContentBody::EditMessage(EditMessage { target_sent_timestamp, data_message: Some(message) }) => {
                let peer = self.peer(message.group_v2.as_ref(), &sender).await;
                let text = self.text(&message, &sender, sent).await.text;
                self.tell(Event::Edited { peer, author: sender, target: target_sent_timestamp.unwrap_or(0), text });
            }
            ContentBody::SynchronizeMessage(sync) => self.sync(sync).await,
            ContentBody::ReceiptMessage(ReceiptMessage { r#type, timestamp }) => {
                if r#type == Some(receipt_message::Type::Read.into()) {
                    self.tell(Event::Seen { by: sender, sent: timestamp });
                }
            }
            ContentBody::TypingMessage(TypingMessage { action, group_id, .. }) => {
                let peer = match group_id {
                    Some(group) => Peer::Group(hex(&group)),
                    None => Peer::Direct(sender.clone()),
                };
                let name = self.name_of(&sender).await;
                let started = action == Some(typing_message::Action::Started.into());
                self.tell(Event::Typing { peer, by: sender, name, started });
            }
            _ => {}
        }
    }

    /// What the user did on another of their devices.
    async fn sync(&mut self, sync: SyncMessage) {
        if let Some(sent) = sync.sent {
            let destination = sent.destination_service_id.as_deref().and_then(ServiceId::parse_from_service_id_string).map(|id| id.raw_uuid().to_string());
            let me = self.me.clone();
            if let Some(message) = &sent.message {
                let peer = self.peer(message.group_v2.as_ref(), destination.as_deref().unwrap_or(&me)).await;
                let at = sent.timestamp.or(message.timestamp).unwrap_or(0);
                self.data(message, &me, peer, at, true).await;
            }
            if let Some(EditMessage { target_sent_timestamp, data_message: Some(message) }) = &sent.edit_message {
                let peer = self.peer(message.group_v2.as_ref(), destination.as_deref().unwrap_or(&me)).await;
                let text = self.text(message, &me, 0).await.text;
                self.tell(Event::Edited { peer, author: me, target: target_sent_timestamp.unwrap_or(0), text });
            }
        }
        if !sync.read.is_empty() {
            let read = sync.read.iter().filter_map(|read| Some((read.sender_aci.clone()?, read.timestamp?))).collect();
            self.tell(Event::ReadElsewhere(read));
        }
        if let Some(configuration) = sync.configuration {
            self.tell(Event::Settings {
                read_receipts: configuration.read_receipts.unwrap_or(false),
                typing: configuration.typing_indicators.unwrap_or(false),
            });
        }
    }

    async fn ask(&mut self, ask: Ask) {
        match ask {
            Ask::Send { to, body, answer } => {
                let _ = answer.send(self.send(&to, body).await);
            }
            Ask::Typing { to, started } => {
                let action = if started { typing_message::Action::Started } else { typing_message::Action::Stopped };
                let at = now_ms();
                let mut typing = TypingMessage { timestamp: Some(at), action: Some(action.into()), group_id: None };
                if let Peer::Group(id) = &to {
                    typing.group_id = unhex(id);
                }
                let _ = self.deliver(&to, ContentBody::TypingMessage(typing), at).await;
            }
            Ask::Read { read, receipts, answer } => {
                let _ = answer.send(self.read(read, receipts).await);
            }
            Ask::Unlink { answer } => {
                let _ = answer.send(self.unlink().await);
            }
        }
    }

    fn group_context(&self, to: &Peer) -> Result<Option<GroupContextV2>, Fault> {
        let Peer::Group(id) = to else {
            return Ok(None);
        };
        let key = self.groups.get(id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let revision = self.vault.group_keys().ok().and_then(|groups| groups.into_iter().find(|(k, _)| k == key)).map_or(0, |(_, group)| group.revision);
        Ok(Some(GroupContextV2 { master_key: Some(key.to_vec()), revision: Some(revision), group_change: None }))
    }

    async fn deliver(&mut self, to: &Peer, body: ContentBody, at: u64) -> Result<(), Fault> {
        match to {
            Peer::Direct(aci) => {
                let uuid = Uuid::parse_str(aci).map_err(|_| Fault::Refused("That person can't be messaged from here.".to_owned()))?;
                self.manager.send_message(Aci::from(uuid), body, at).await.map_err(fault)
            }
            Peer::Group(id) => {
                let key = *self.groups.get(id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
                self.manager.send_message_to_group(&key, body, at).await.map_err(fault)
            }
        }
    }

    async fn send(&mut self, to: &Peer, body: Body) -> Result<u64, Fault> {
        let at = now_ms();
        let group_v2 = self.group_context(to)?;
        let content = match body {
            Body::Text { text, quote } => ContentBody::DataMessage(DataMessage {
                body: Some(text),
                quote: quote.map(|quote| data_message::Quote {
                    id: Some(quote.sent),
                    author_aci: Some(quote.author),
                    text: Some(quote.text),
                    r#type: Some(data_message::quote::Type::Normal.into()),
                    ..Default::default()
                }),
                group_v2,
                ..Default::default()
            }),
            Body::React { author, target, emoji, remove } => ContentBody::DataMessage(DataMessage {
                reaction: Some(data_message::Reaction {
                    emoji: Some(emoji),
                    remove: Some(remove),
                    target_author_aci: Some(author),
                    target_sent_timestamp: Some(target),
                    ..Default::default()
                }),
                group_v2,
                ..Default::default()
            }),
            Body::Delete { target } => ContentBody::DataMessage(DataMessage {
                delete: Some(data_message::Delete { target_sent_timestamp: Some(target) }),
                group_v2,
                ..Default::default()
            }),
            Body::Edit { target, text } => ContentBody::EditMessage(EditMessage {
                target_sent_timestamp: Some(target),
                data_message: Some(DataMessage { body: Some(text), timestamp: Some(at), group_v2, ..Default::default() }),
            }),
        };
        self.deliver(to, content, at).await?;
        Ok(at)
    }

    /// Read receipts to each author (when the user's settings send them), and one read sync to
    /// the user's other devices.
    async fn read(&mut self, read: Vec<(String, u64)>, receipts: bool) -> Result<(), Fault> {
        if receipts {
            let mut by_author: HashMap<String, Vec<u64>> = HashMap::new();
            for (author, sent) in &read {
                by_author.entry(author.clone()).or_default().push(*sent);
            }
            for (author, timestamp) in by_author {
                let receipt = ReceiptMessage { r#type: Some(receipt_message::Type::Read.into()), timestamp };
                // A receipt that does not go is not worth failing the read for.
                let _ = self.deliver(&Peer::Direct(author), ContentBody::ReceiptMessage(receipt), now_ms()).await;
            }
        }
        let sync = SyncMessage {
            read: read.into_iter().map(|(author, sent)| sync_message::Read { sender_aci: Some(author), timestamp: Some(sent), ..Default::default() }).collect(),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        let me = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        self.manager.send_message(Aci::from(me), sync, now_ms()).await.map_err(fault)
    }

    /// Removes this device from the account, as the phone's own list would. The pinned presage
    /// lets only a primary device do it, so this is the same request made on this device's own
    /// socket: the server lets a linked device remove itself.
    async fn unlink(&mut self) -> Result<(), Fault> {
        let data: &RegistrationData = self.manager.registration_data();
        let password = self.vault.password().ok_or_else(|| Fault::Net("no password kept".into()))?;
        let phone = (&data.phone_number).try_into_e164().map_err(|err| Fault::Net(err.to_string()))?;
        let device = self.manager.device_id();
        let credentials = ServiceCredentials {
            aci: Some(data.service_ids.aci),
            pni: Some(data.service_ids.pni),
            phonenumber: phone,
            password: Some(password),
            device_id: Some(device),
        };
        let mut push = PushService::new(SignalServers::Production, Some(credentials.clone()), "tinyclient");
        let mut socket = push.ws::<Identified>("/v1/websocket/", "/v1/keepalive", &[], Some(credentials)).await.map_err(|err| fault::<vault::VaultError>(err.into()))?;
        socket.unlink_device(device).await.map_err(|err| fault::<vault::VaultError>(err.into()))?;
        vault::remove(self.vault.path());
        Ok(())
    }
}

/// Links this computer to an account, as Signal Desktop is linked.
pub struct RealLinker;

impl Linker for RealLinker {
    fn link(self: Box<Self>, device: &str, show: &dyn Fn(String), cancelled: Arc<AtomicBool>) -> Result<Linked, Fault> {
        let key = vault::new_key().map_err(Fault::Net)?;
        let pending = vault::path_of(&format!("linking-{}", hex(&key[..6])));
        vault::remove(&pending);
        let store = Vault::open(&pending, &key).map_err(Fault::Net)?;
        let runtime = runtime().map_err(Fault::Net)?;
        let local = tokio::task::LocalSet::new();
        let (shown_tx, shown_rx) = futures::channel::oneshot::channel();
        let linked = local.block_on(&runtime, async {
            let link = Manager::link_secondary_device(store.clone(), SignalServers::Production, device.to_owned(), shown_tx);
            let show_link = async {
                if let Ok(url) = shown_rx.await {
                    show(url.to_string());
                }
                futures::future::pending::<()>().await
            };
            let watch = async {
                while !cancelled.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            };
            tokio::select! {
                linked = link => Some(linked),
                _ = show_link => None,
                _ = watch => None,
            }
        });
        let mut manager = match linked {
            Some(Ok(manager)) => manager,
            Some(Err(err)) => {
                drop(store);
                vault::remove(&pending);
                return Err(fault(err));
            }
            None => {
                drop(store);
                vault::remove(&pending);
                return Err(Fault::Net("cancelled".into()));
            }
        };
        let aci = manager.registration_data().service_ids.aci.to_string();
        let name = local.block_on(&runtime, own_name(&mut manager)).unwrap_or_default();
        // Closed before the user saw it finish: undone, never left on the account.
        if cancelled.load(Ordering::Relaxed) {
            let mut session = Session { manager, vault: store.clone(), me: aci, events: channel().0, names: HashMap::new(), groups: HashMap::new() };
            let _ = local.block_on(&runtime, session.unlink());
            drop(session);
            vault::remove(&pending);
            return Err(Fault::Net("cancelled".into()));
        }
        drop(manager);
        drop(store);
        let home = vault::path_of(&aci);
        vault::remove(&home);
        std::fs::rename(&pending, &home).map_err(|err| Fault::Net(err.to_string()))?;
        let saved = Saved { aci, name, vault: hex(&key) };
        let session = serde_json::to_string(&saved).map_err(|err| Fault::Net(err.to_string()))?;
        Ok(Linked { session })
    }
}
