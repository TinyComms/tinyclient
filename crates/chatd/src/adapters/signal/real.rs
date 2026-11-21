//! The wire to Signal itself: presage, on one current-thread tokio runtime and its `LocalSet`, on a
//! thread of the wire's own. The adapter's threads ask it things over a channel and wait for the
//! answer; what Signal says unasked becomes `Event`s.
//!
//! Built only with the `signal` feature. Untested against a live account: no test can reach
//! Signal without a real phone to link from (see `tests/signal.rs` for what is proven).

use super::style::{self, Mark, Range};
use super::vault::{self, Vault};
use super::wire::{
    call_link_room_id, format_call_link, format_group_invite_link, gift_badge_label, parse_call_link,
    parse_group_invite_link, payment_label, view_once_label, Body, CallKind, CallLink, CallLinkJoin, CallLogKind,
    CallOutcome, ChatFolder, Contact, CONTINUES, Event, Fault, GifEntry, GroupAccess, GroupEdit, GroupInfo,
    GroupMember, GroupMembers, IdentityState, InviteLinkMode, Linked, Linker, MessageRequestAction, OwnCard, Peer,
    PersonProfile, Quote, SafetyNumber, StorageAccount, StorageContact, StorageGroup, StorageSnapshot, Text, Wire,
};
use super::{group_delta, GroupRevisions, GroupSnap, MemberSnap, Saved};
use futures::StreamExt;
use presage::libsignal_service::content::ContentBody;
use presage::libsignal_service::groups_v2::{AccessControl, AccessRequired, Member, Role};
use presage::libsignal_service::libsignal_account_keys::AccountEntropyPool;
use presage::libsignal_service::master_key::StorageServiceKey;
use presage::libsignal_service::prelude::{Content, GroupMasterKey, GroupSecretParams, MasterKey, ProfileKey, PushService, ServiceCredentials, ServiceError, Uuid};
use presage::libsignal_service::profile_cipher::ProfileCipher;
use presage::libsignal_service::profile_name::ProfileName;
use presage::libsignal_service::protocol::{Aci, ServiceId};
use presage::libsignal_service::sender::{AttachmentSpec, AttachmentUploadError, MessageSenderError};
use presage::libsignal_service::StorageService;
use presage::libsignal_service::utils::TryIntoE164;
use presage::libsignal_service::websocket::Identified;
use presage::libsignal_service::configuration::SignalServers;
use presage::manager::{Registered, RegistrationData};
use presage::model::groups::Group;
use presage::model::messages::Received;
use presage::proto::{
    account_record, addressable_message,
    body_range::{AssociatedValue, Style as RangeStyle},
    call_message, chat_folder_record, contact_record, conversation_identifier, data_message, receipt_message, recipient,
    storage_record, sync_message, typing_message, verified, AddressableMessage, BodyRange, CallMessage,
    ConversationIdentifier, DataMessage, EditMessage, GroupContextV2, ReceiptMessage, SyncMessage, TypingMessage,
    Verified,
};
use presage::store::{ContentsStore, StateStore};
use presage::Manager;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc as async_mpsc;

/// How long the adapter waits for one answer from the wire's thread.
const ANSWER: Duration = Duration::from_secs(45);
/// How long connecting may take before the attempt counts as dropped.
const CONNECT: Duration = Duration::from_secs(60);
/// Signal's content type for text too long for one message: the whole text travels as this
/// attachment, and the body repeats its first part.
const LONG_TEXT: &str = "text/x-signal-plain";

type Signal = Manager<Vault, Registered>;

/// What the adapter's threads ask of the wire's thread.
enum Ask {
    Send { to: Peer, body: Body, answer: Sender<Result<u64, Fault>> },
    Typing { to: Peer, started: bool },
    Read { read: Vec<(String, u64)>, receipts: bool, answer: Sender<Result<(), Fault>> },
    Unlink { answer: Sender<Result<(), Fault>> },
    Picture { of: Peer, answer: Sender<Result<Option<Vec<u8>>, Fault>> },
    Card { answer: Sender<Result<OwnCard, Fault>> },
    SetCard { name: Option<String>, about: Option<String>, emoji: Option<String>, answer: Sender<Result<(), Fault>> },
    Lookup { username: String, answer: Sender<Result<Option<String>, Fault>> },
    SetMute { _to: Peer, _until_ms: u64, answer: Sender<Result<(), Fault>> },
    SetArchived { _to: Peer, _archived: bool, answer: Sender<Result<(), Fault>> },
    SetPinned { _to: Peer, _pinned: bool, answer: Sender<Result<(), Fault>> },
    SetMarkedUnread { _to: Peer, _unread: bool, answer: Sender<Result<(), Fault>> },
    SetBlocked { peer: Peer, blocked: bool, answer: Sender<Result<(), Fault>> },
    RespondMessageRequest { peer: Peer, action: MessageRequestAction, answer: Sender<Result<(), Fault>> },
    CreateGroup { title: String, members: Vec<String>, answer: Sender<Result<GroupInfo, Fault>> },
    LeaveGroup { group_id: String, answer: Sender<Result<(), Fault>> },
    GroupMembers { group_id: String, answer: Sender<Result<GroupMembers, Fault>> },
    GroupAddMembers { group_id: String, people: Vec<String>, answer: Sender<Result<(), Fault>> },
    GroupRemoveMember { group_id: String, person: String, answer: Sender<Result<(), Fault>> },
    GroupSetAdmin { group_id: String, person: String, admin: bool, answer: Sender<Result<(), Fault>> },
    GroupUpdate { group_id: String, edit: GroupEdit, answer: Sender<Result<(), Fault>> },
    GroupSetAvatar { group_id: String, bytes: Vec<u8>, answer: Sender<Result<(), Fault>> },
    GroupSetTimer { group_id: String, seconds: u32, answer: Sender<Result<(), Fault>> },
    GroupInviteLink { group_id: String, mode: InviteLinkMode, answer: Sender<Result<String, Fault>> },
    GroupJoinByLink { url: String, answer: Sender<Result<GroupInfo, Fault>> },
    GroupReviewRequest { group_id: String, person: String, approve: bool, answer: Sender<Result<(), Fault>> },
    GroupPendingInvite { group_id: String, accept: bool, answer: Sender<Result<(), Fault>> },
    CreateCallLink { name: String, approve_all: bool, answer: Sender<Result<CallLink, Fault>> },
    UpdateCallLink { root_key: [u8; 32], name: Option<String>, approve_all: Option<bool>, answer: Sender<Result<CallLink, Fault>> },
    DeleteCallLink { root_key: [u8; 32], answer: Sender<Result<(), Fault>> },
    JoinCallLink { url: String, answer: Sender<Result<CallLinkJoin, Fault>> },
    SafetyNumber { aci: String, answer: Sender<Result<Option<SafetyNumber>, Fault>> },
    SetVerified { aci: String, state: IdentityState, answer: Sender<Result<(), Fault>> },
    PersonProfile { aci: String, answer: Sender<Result<PersonProfile, Fault>> },
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

/// The conversation a delete-for-me names. A group is its public id, not its master key.
fn conversation_of(to: &Peer) -> Result<ConversationIdentifier, Fault> {
    let identifier = match to {
        Peer::Direct(aci) => conversation_identifier::Identifier::ThreadServiceId(aci.clone()),
        Peer::Group(id) => {
            let bytes = unhex(id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
            conversation_identifier::Identifier::ThreadGroupId(bytes)
        }
    };
    Ok(ConversationIdentifier { identifier: Some(identifier) })
}

fn addressable(author: &str, sent: u64) -> AddressableMessage {
    AddressableMessage {
        author: Some(addressable_message::Author::AuthorServiceId(author.to_owned())),
        sent_timestamp: Some(sent),
        ..Default::default()
    }
}

fn peer_of(conversation: Option<ConversationIdentifier>) -> Option<Peer> {
    match conversation.and_then(|id| id.identifier) {
        Some(conversation_identifier::Identifier::ThreadServiceId(aci)) if super::wire::is_aci(&aci) => Some(Peer::Direct(aci)),
        Some(conversation_identifier::Identifier::ThreadGroupId(id)) => Some(Peer::Group(hex(&id))),
        Some(conversation_identifier::Identifier::ThreadServiceIdBinary(bytes)) => aci_bytes(&bytes).map(Peer::Direct),
        _ => None,
    }
}

fn addressed(message: &AddressableMessage) -> Option<(String, u64)> {
    let sent = message.sent_timestamp?;
    let author = match message.author.as_ref()? {
        addressable_message::Author::AuthorServiceId(aci) if super::wire::is_aci(aci) => aci.clone(),
        addressable_message::Author::AuthorServiceIdBinary(bytes) => aci_bytes(bytes)?,
        _ => return None,
    };
    Some((author, sent))
}

/// A 16-byte ACI, as the delete-for-me sync writes one when it does not use the string form.
fn aci_bytes(bytes: &[u8]) -> Option<String> {
    let raw: [u8; 16] = bytes.try_into().ok()?;
    let aci = Uuid::from_bytes(raw).to_string();
    super::wire::is_aci(&aci).then_some(aci)
}

/// A group's public id: what the contract and typing messages name it by. Its master key opens
/// the group and stays in the vault.
fn group_id(master_key: &[u8; 32]) -> String {
    hex(&GroupSecretParams::derive_from_master_key(GroupMasterKey::new(*master_key)).get_group_identifier())
}

fn proto_access(access: GroupAccess) -> AccessRequired {
    match access {
        GroupAccess::Any => AccessRequired::Any,
        GroupAccess::Member => AccessRequired::Member,
        GroupAccess::Administrator => AccessRequired::Administrator,
        GroupAccess::Unsatisfiable => AccessRequired::Unsatisfiable,
    }
}

fn access_of(req: AccessRequired) -> GroupAccess {
    match req {
        AccessRequired::Any => GroupAccess::Any,
        AccessRequired::Member => GroupAccess::Member,
        AccessRequired::Administrator => GroupAccess::Administrator,
        AccessRequired::Unsatisfiable | AccessRequired::Unknown => GroupAccess::Unsatisfiable,
    }
}

/// The stored group, as a change compares it. Presage fetched this; the message's unsigned change is not read.
fn snap_of(group: &Group) -> GroupSnap {
    let mut members: Vec<MemberSnap> = group
        .members
        .iter()
        .map(|member| MemberSnap { id: member.aci.service_id_string(), admin: member.role == Role::Administrator })
        .collect();
    members.sort_by(|left, right| left.id.cmp(&right.id));
    let mut pending: Vec<String> = group.pending_members.iter().map(|member| member.uuid.to_string()).collect();
    pending.sort();
    let mut requesting: Vec<String> = group.requesting_members.iter().map(|member| member.aci.service_id_string()).collect();
    requesting.sort();
    let link = group.access_control.as_ref().is_some_and(|access| access.add_from_invite_link == AccessRequired::Any);
    GroupSnap {
        title: group.title.clone(),
        description: group.description.clone().unwrap_or_default(),
        timer_secs: group.disappearing_messages_timer.as_ref().map(|timer| timer.duration).unwrap_or(0),
        avatar: group.avatar.clone(),
        members,
        pending,
        requesting,
        link,
    }
}

/// Ids whose membership, role, invite or request changed. A rename does not name a person.
fn people_who_changed(before: &GroupSnap, after: &GroupSnap) -> Vec<String> {
    let was: HashMap<&str, bool> = before.members.iter().map(|member| (member.id.as_str(), member.admin)).collect();
    let now: HashMap<&str, bool> = after.members.iter().map(|member| (member.id.as_str(), member.admin)).collect();
    let mut ids: Vec<&str> = Vec::new();
    for id in now.keys() {
        if was.get(id) != now.get(id) {
            ids.push(*id);
        }
    }
    for id in was.keys() {
        if !now.contains_key(id) {
            ids.push(*id);
        }
    }
    let members_now: HashSet<&str> = now.keys().copied().collect();
    let pending: HashSet<&str> = before.pending.iter().map(String::as_str).collect();
    for id in &after.pending {
        if !pending.contains(id.as_str()) && !members_now.contains(id.as_str()) {
            ids.push(id);
        }
    }
    let asking: HashSet<&str> = before.requesting.iter().map(String::as_str).collect();
    for id in &after.requesting {
        if !asking.contains(id.as_str()) {
            ids.push(id);
        }
    }
    let mut owned: Vec<String> = ids.into_iter().map(str::to_owned).collect();
    owned.sort();
    owned.dedup();
    owned
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
        (_, Some(MessageSenderError::UntrustedIdentity { .. }))
        | (_, Some(MessageSenderError::ProtocolError(presage::libsignal_service::protocol::SignalProtocolError::UntrustedIdentity(_)))) => {
            Fault::Refused(
                "Your safety number with this contact has changed. Review and approve their safety number before sending.".to_owned(),
            )
        }
        (P::ServiceError(ServiceError::RateLimitExceeded { .. }), _) => Fault::Refused("Signal asked for a pause. Try again in a minute.".to_owned()),
        (P::UnknownGroup, _) => Fault::Refused("This group isn't one Signal still knows for you.".to_owned()),
        _ => Fault::Net(err.to_string()),
    }
}

/// What of a payment can be shown beside its label: the note its sender typed, when they did.
/// Nothing else of it is readable — the amount rides inside an encrypted receipt only a wallet
/// can open, and this is not one (#733).
fn payment_note(payment: &data_message::Payment) -> Option<&str> {
    match &payment.item {
        Some(data_message::payment::Item::Notification(notification)) => notification.note.as_deref(),
        _ => None,
    }
}

/// What a message that is not drawn here says, in words. A view-once photo or video is named
/// before its attachment: it opens once, on the phone, so "Photo" would promise a file that is
/// not here to open and say nothing of why (#748). A long text is drawn: it is the message's own
/// words, not a file (#728).
fn unsupported_label(message: &DataMessage) -> Option<String> {
    if message.is_view_once.unwrap_or(false) {
        let kind = message.attachments.first().and_then(|attachment| attachment.content_type.as_deref());
        return Some(view_once_label(kind).to_owned());
    }
    if long_text(message) {
        return None;
    }
    if let Some(attachment) = message.attachments.first() {
        if attachment.flags.unwrap_or(0) & 8 != 0 {
            return Some("GIF".to_owned());
        }
        let kind = attachment.content_type.as_deref().unwrap_or("");
        return Some(
            match kind.split('/').next() {
                Some("image") => "Photo",
                Some("video") => "Video",
                Some("audio") => "Audio",
                _ => "File",
            }
            .to_owned(),
        );
    }
    if message.sticker.is_some() {
        return Some("Sticker".to_owned());
    }
    if let Some(payment) = message.payment.as_ref() {
        return Some(payment_label(payment_note(payment)));
    }
    if message.gift_badge.is_some() {
        return Some(gift_badge_label().to_owned());
    }
    None
}

/// An attachment Signal did not take, in words that name the file and not the address.
fn attachment_refused(name: &str, err: &AttachmentUploadError) -> Fault {
    match err {
        AttachmentUploadError::IoError(_) => Fault::Refused(format!("{name} could not be read.")),
        AttachmentUploadError::ServiceError(_) => Fault::Refused(format!("{name} could not be sent.")),
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
        super::presage_thread("signal-wire")
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
                    let (id_tx, mut id_rx) = async_mpsc::unbounded_channel();
                    vault.set_identity_change_sink(id_tx);
                    let mut wire = Session { manager, vault, me: aci.clone(), events, names: HashMap::new(), groups: HashMap::new(), known: GroupRevisions::default(), blocked_acis: HashSet::new(), blocked_groups: HashSet::new() };
                    let _ = ready.send(Ok((aci, name)));
                    wire.first_run().await;
                    wire.groups_changed().await;
                    let why = loop {
                        tokio::select! {
                            received = messages.next() => match received {
                                Some(received) => wire.received(received).await,
                                None => break wire.why_closed().await,
                            },
                            Some(changed_aci) = id_rx.recv() => {
                                if super::wire::is_aci(&changed_aci) {
                                    wire.tell(Event::IdentityChanged { peer: Peer::Direct(changed_aci) });
                                }
                            }
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

    fn picture(&self, of: &Peer) -> Result<Option<Vec<u8>>, Fault> {
        self.ask(|answer| Ask::Picture { of: of.clone(), answer })
    }

    fn card(&self) -> Result<OwnCard, Fault> {
        self.ask(|answer| Ask::Card { answer })
    }

    fn set_card(&self, name: Option<String>, about: Option<String>, emoji: Option<String>) -> Result<(), Fault> {
        self.ask(|answer| Ask::SetCard { name, about, emoji, answer })
    }

    fn lookup(&self, username: &str) -> Result<Option<String>, Fault> {
        self.ask(|answer| Ask::Lookup { username: username.to_owned(), answer })
    }

    fn set_mute(&self, to: &Peer, until_ms: u64) -> Result<(), Fault> {
        self.ask(|answer| Ask::SetMute { _to: to.clone(), _until_ms: until_ms, answer })
    }

    fn set_archived(&self, to: &Peer, archived: bool) -> Result<(), Fault> {
        self.ask(|answer| Ask::SetArchived { _to: to.clone(), _archived: archived, answer })
    }

    fn set_pinned(&self, to: &Peer, pinned: bool) -> Result<(), Fault> {
        self.ask(|answer| Ask::SetPinned { _to: to.clone(), _pinned: pinned, answer })
    }

    fn set_marked_unread(&self, to: &Peer, unread: bool) -> Result<(), Fault> {
        self.ask(|answer| Ask::SetMarkedUnread { _to: to.clone(), _unread: unread, answer })
    }

    fn set_blocked(&self, peer: &Peer, blocked: bool) -> Result<(), Fault> {
        self.ask(|answer| Ask::SetBlocked { peer: peer.clone(), blocked, answer })
    }

    fn respond_message_request(&self, peer: &Peer, action: MessageRequestAction) -> Result<(), Fault> {
        self.ask(|answer| Ask::RespondMessageRequest { peer: peer.clone(), action, answer })
    }

    fn create_group(&self, title: &str, members: &[String]) -> Result<GroupInfo, Fault> {
        self.ask(|answer| Ask::CreateGroup { title: title.to_owned(), members: members.to_vec(), answer })
    }

    fn leave_group(&self, group_id: &str) -> Result<(), Fault> {
        self.ask(|answer| Ask::LeaveGroup { group_id: group_id.to_owned(), answer })
    }

    fn group_members(&self, group_id: &str) -> Result<GroupMembers, Fault> {
        self.ask(|answer| Ask::GroupMembers { group_id: group_id.to_owned(), answer })
    }

    fn group_add_members(&self, group_id: &str, people: &[String]) -> Result<(), Fault> {
        self.ask(|answer| Ask::GroupAddMembers { group_id: group_id.to_owned(), people: people.to_vec(), answer })
    }

    fn group_remove_member(&self, group_id: &str, person: &str) -> Result<(), Fault> {
        self.ask(|answer| Ask::GroupRemoveMember { group_id: group_id.to_owned(), person: person.to_owned(), answer })
    }

    fn group_set_admin(&self, group_id: &str, person: &str, admin: bool) -> Result<(), Fault> {
        self.ask(|answer| Ask::GroupSetAdmin { group_id: group_id.to_owned(), person: person.to_owned(), admin, answer })
    }

    fn group_update(&self, group_id: &str, edit: GroupEdit) -> Result<(), Fault> {
        self.ask(|answer| Ask::GroupUpdate { group_id: group_id.to_owned(), edit, answer })
    }

    fn group_set_avatar(&self, group_id: &str, bytes: Vec<u8>) -> Result<(), Fault> {
        self.ask(|answer| Ask::GroupSetAvatar { group_id: group_id.to_owned(), bytes, answer })
    }

    fn group_set_timer(&self, group_id: &str, seconds: u32) -> Result<(), Fault> {
        self.ask(|answer| Ask::GroupSetTimer { group_id: group_id.to_owned(), seconds, answer })
    }

    fn group_invite_link(&self, group_id: &str, mode: InviteLinkMode) -> Result<String, Fault> {
        self.ask(|answer| Ask::GroupInviteLink { group_id: group_id.to_owned(), mode, answer })
    }

    fn group_join_by_link(&self, url: &str) -> Result<GroupInfo, Fault> {
        self.ask(|answer| Ask::GroupJoinByLink { url: url.to_owned(), answer })
    }

    fn group_review_request(&self, group_id: &str, person: &str, approve: bool) -> Result<(), Fault> {
        self.ask(|answer| Ask::GroupReviewRequest { group_id: group_id.to_owned(), person: person.to_owned(), approve, answer })
    }

    fn group_pending_invite(&self, group_id: &str, accept: bool) -> Result<(), Fault> {
        self.ask(|answer| Ask::GroupPendingInvite { group_id: group_id.to_owned(), accept, answer })
    }

    fn search_gifs(&self, query: &str, offset: u32) -> Result<(Vec<GifEntry>, Option<u32>), Fault> {
        let base = std::env::var("TINYCLIENT_SIGNAL_GIPHY_PROXY")
            .unwrap_or_else(|_| "https://giphy-proxy-production.whispersystems.org".to_owned());
        let base = base.trim_end_matches('/');
        let q = query.trim();
        let url = if q.is_empty() {
            format!("{base}/v1/gifs/trending?api_key=37SiWz384431444&limit=25&offset={offset}&rating=pg-13")
        } else {
            let encoded: String = q
                .bytes()
                .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { (b as char).to_string() } else { format!("%{b:02X}") })
                .collect();
            format!("{base}/v1/gifs/search?api_key=37SiWz384431444&q={encoded}&limit=25&offset={offset}&rating=pg-13")
        };
        let mut resp = ureq::get(&url)
            .header("User-Agent", "Signal-Desktop")
            .call()
            .map_err(|err| Fault::Net(err.to_string()))?;
        let json: serde_json::Value = resp.body_mut().read_json().map_err(|err| Fault::Net(err.to_string()))?;
        let data = json.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let Some(id) = item.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                continue;
            };
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let images = item.get("images");
            let fixed = images.and_then(|i| i.get("fixed_width"));
            let preview_url = images
                .and_then(|i| i.get("fixed_width_small_still"))
                .or(fixed)
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let mp4_url = fixed
                .and_then(|v| v.get("mp4"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&preview_url)
                .to_owned();
            if mp4_url.is_empty() {
                continue;
            }
            let width = fixed.and_then(|v| v.get("width")).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
            let height = fixed.and_then(|v| v.get("height")).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
            out.push(GifEntry { id: id.to_owned(), title, preview_url, mp4_url, width, height });
        }
        let next = if out.len() >= 25 { Some(offset + out.len() as u32) } else { None };
        Ok((out, next))
    }

    fn fetch_gif(&self, url: &str) -> Result<Vec<u8>, Fault> {
        let mut resp = ureq::get(url)
            .header("User-Agent", "Signal-Desktop")
            .call()
            .map_err(|err| Fault::Net(err.to_string()))?;
        let bytes = resp.body_mut().read_to_vec().map_err(|err| Fault::Net(err.to_string()))?;
        Ok(bytes)
    }

    fn create_call_link(&self, name: &str, approve_all: bool) -> Result<CallLink, Fault> {
        self.ask(|answer| Ask::CreateCallLink { name: name.to_owned(), approve_all, answer })
    }

    fn update_call_link(&self, root_key: &[u8; 32], name: Option<String>, approve_all: Option<bool>) -> Result<CallLink, Fault> {
        self.ask(|answer| Ask::UpdateCallLink { root_key: *root_key, name, approve_all, answer })
    }

    fn delete_call_link(&self, root_key: &[u8; 32]) -> Result<(), Fault> {
        self.ask(|answer| Ask::DeleteCallLink { root_key: *root_key, answer })
    }

    fn join_call_link(&self, url: &str) -> Result<CallLinkJoin, Fault> {
        self.ask(|answer| Ask::JoinCallLink { url: url.to_owned(), answer })
    }

    fn safety_number(&self, aci: &str) -> Result<Option<SafetyNumber>, Fault> {
        self.ask(|answer| Ask::SafetyNumber { aci: aci.to_owned(), answer })
    }

    fn set_verified(&self, aci: &str, state: IdentityState) -> Result<(), Fault> {
        self.ask(|answer| Ask::SetVerified { aci: aci.to_owned(), state, answer })
    }

    fn person_profile(&self, aci: &str) -> Result<PersonProfile, Fault> {
        self.ask(|answer| Ask::PersonProfile { aci: aci.to_owned(), answer })
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

/// Whether the message's text is too long for one message: Signal carries the whole text as
/// its own plain-text attachment, the first part of which the body repeats. Until attachments
/// can be fetched, the body is all this device has of it.
fn long_text(message: &DataMessage) -> bool {
    message.attachments.first().is_some_and(|attachment| attachment.content_type.as_deref() == Some(LONG_TEXT))
}

/// The words of a message before names are put to its mentions. A long text's body carries the
/// note that the rest follows on the phone.
fn words(message: &DataMessage) -> String {
    let mut text = message.body.clone().unwrap_or_default();
    if long_text(message) {
        text.push_str(CONTINUES);
    }
    text
}

/// The lab's signal-long-text driver: a long text is the message's own words, not a file, and
/// its body carries the note that the rest is on the phone.
pub fn prove_long_text() -> Result<(), String> {
    let body = "x".repeat(2_048);
    let mut long = DataMessage::default();
    long.body = Some(body.clone());
    long.attachments.push(Default::default());
    if let Some(attachment) = long.attachments.last_mut() {
        attachment.content_type = Some(LONG_TEXT.to_owned());
    }
    if unsupported_label(&long).is_some() {
        return Err("a long text was named like a file".into());
    }
    if words(&long) != format!("{body}{CONTINUES}") {
        return Err("a long text was shown without the note".into());
    }
    let mut photo = DataMessage::default();
    photo.attachments.push(Default::default());
    if let Some(attachment) = photo.attachments.last_mut() {
        attachment.content_type = Some("image/jpeg".to_owned());
    }
    if unsupported_label(&photo).as_deref() != Some("Photo") {
        return Err("a photo was no longer named a photo".into());
    }
    Ok(())
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
    /// Last revision applied for a group, and the snapshot behind it. The first one is not announced.
    known: GroupRevisions,
    /// Currently blocked ACIs and group ids, so a `SyncMessage.blocked` write sends the full list.
    blocked_acis: HashSet<String>,
    blocked_groups: HashSet<String>,
}

impl Session {
    fn tell(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Once per link: ask the phone for the contacts and the settings it shares with devices.
    async fn first_run(&mut self) {
        if self.vault.flag("asked") {
            self.tell(Event::Contacts(self.contacts().await));
            self.sync_storage().await;
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
        self.sync_storage().await;
    }

    /// Derives the account's read-only Storage Service key from the stored master key or account entropy pool.
    async fn storage_key(&self) -> Option<StorageServiceKey> {
        if let Ok(Some(master_key)) = self.vault.fetch_master_key().await {
            return Some(StorageServiceKey::from_master_key(&master_key));
        }
        if let Ok(Some(aep)) = self.vault.fetch_account_entropy_pool().await {
            if let Ok(master_key) = MasterKey::from_slice(aep.derive_svr_key().as_slice()) {
                let _ = self.vault.store_master_key(Some(&master_key)).await;
                return Some(StorageServiceKey::from_master_key(&master_key));
            }
        }
        None
    }

    /// Reads Signal's encrypted Storage Service manifest and items (contact, group, account, folder records)
    /// in read-only mode to seed contacts, groups, pins, archive, mutes, nicknames, notes, and settings.
    async fn sync_storage(&mut self) {
        let Some(storage_key) = self.storage_key().await else {
            return;
        };
        let Ok(credentials) = self.credentials() else {
            return;
        };
        let push = PushService::new(SignalServers::Production, Some(credentials), "tinyclient");
        let Ok(storage) = StorageService::new(push, storage_key).await else {
            return;
        };
        let Ok(manifest) = storage.manifest().await else {
            return;
        };
        let keys: Vec<Vec<u8>> = manifest.identifiers.into_iter().map(|id| id.raw).collect();
        if keys.is_empty() {
            return;
        }
        let ikm = (!manifest.record_ikm.is_empty()).then_some(manifest.record_ikm.as_slice());
        let Ok(records) = storage.read_items(keys, ikm).await else {
            return;
        };
        let join_name = |given: &str, family: &str| -> String {
            [given.trim(), family.trim()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut contacts = Vec::new();
        let mut groups = Vec::new();
        let mut account_record: Option<StorageAccount> = None;
        let mut folders = Vec::new();
        for record in records {
            match record.record {
                Some(storage_record::Record::Contact(contact)) => {
                    let aci = if !contact.aci.is_empty() && super::wire::is_aci(&contact.aci) {
                        Some(contact.aci.clone())
                    } else if !contact.aci_binary.is_empty() {
                        aci_bytes(&contact.aci_binary)
                    } else {
                        None
                    };
                    if let Some(aci) = aci {
                        if contact.blocked {
                            self.blocked_acis.insert(aci.clone());
                        } else {
                            self.blocked_acis.remove(&aci);
                        }
                        if let (Ok(uuid), Ok(pk_bytes)) = (Uuid::parse_str(&aci), <[u8; 32]>::try_from(contact.profile_key.as_slice())) {
                            let _ = self.vault.upsert_profile_key(&uuid, ProfileKey::create(pk_bytes)).await;
                        }
                        let profile_name = join_name(&contact.given_name, &contact.family_name);
                        let system_name = join_name(&contact.system_given_name, &contact.system_family_name);
                        let name = if !profile_name.is_empty() { profile_name } else { system_name };
                        let nickname = contact
                            .nickname
                            .as_ref()
                            .map(|n| join_name(&n.given, &n.family))
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| contact.system_nickname.trim().to_owned());
                        let identity_state = match contact_record::IdentityState::try_from(contact.identity_state) {
                            Ok(contact_record::IdentityState::Verified) => IdentityState::Verified,
                            Ok(contact_record::IdentityState::Unverified) => IdentityState::Unverified,
                            _ => IdentityState::Default,
                        };
                        self.vault.sync_storage_identity(&aci, identity_state, &contact.identity_key);
                        contacts.push(StorageContact {
                            aci,
                            name,
                            nickname,
                            note: contact.note.trim().to_owned(),
                            phone: contact.e164.trim().to_owned(),
                            username: contact.username.trim().to_owned(),
                            identity_state,
                            blocked: contact.blocked,
                            whitelisted: contact.whitelisted,
                            archived: contact.archived,
                            marked_unread: contact.marked_unread,
                            muted_until: contact.muted_until_timestamp,
                            hidden: contact.hidden,
                        });
                    }
                }
                Some(storage_record::Record::GroupV2(group)) => {
                    if let Ok(key) = <[u8; 32]>::try_from(group.master_key.as_slice()) {
                        let id = group_id(&key);
                        self.groups.entry(id.clone()).or_insert(key);
                        if group.blocked {
                            self.blocked_groups.insert(id.clone());
                        } else {
                            self.blocked_groups.remove(&id);
                        }
                        let title = self
                            .vault
                            .group(key)
                            .await
                            .ok()
                            .flatten()
                            .map(|g| g.title)
                            .unwrap_or_default();
                        groups.push(StorageGroup {
                            id,
                            title,
                            blocked: group.blocked,
                            whitelisted: group.whitelisted,
                            archived: group.archived,
                            marked_unread: group.marked_unread,
                            muted_until: group.muted_until_timestamp,
                            dont_notify_for_mentions_if_muted: group.dont_notify_for_mentions_if_muted,
                        });
                    }
                }
                Some(storage_record::Record::Account(account)) => {
                    let mut pins = Vec::new();
                    for item in account.pinned_conversations {
                        match item.identifier {
                            Some(account_record::pinned_conversation::Identifier::Contact(contact)) => {
                                if super::wire::is_aci(&contact.service_id) {
                                    pins.push(contact.service_id);
                                } else if let Some(aci) = aci_bytes(&contact.service_id_binary) {
                                    pins.push(aci);
                                }
                            }
                            Some(account_record::pinned_conversation::Identifier::GroupMasterKey(raw)) => {
                                if let Ok(key) = <[u8; 32]>::try_from(raw.as_slice()) {
                                    pins.push(Peer::Group(group_id(&key)).conversation());
                                }
                            }
                            _ => {}
                        }
                    }
                    account_record = Some(StorageAccount {
                        pinned: pins,
                        keep_muted_archived: Some(account.keep_muted_chats_archived),
                        link_previews: Some(account.link_previews),
                        universal_expire_timer: account.universal_expire_timer,
                        note_to_self_marked_unread: account.note_to_self_marked_unread,
                    });
                }
                Some(storage_record::Record::ChatFolder(folder)) => {
                    if folder.deleted_at_timestamp_ms != 0
                        || folder.folder_type == chat_folder_record::FolderType::All as i32
                        || folder.name.trim().is_empty()
                    {
                        continue;
                    }
                    let id = if folder.identifier.is_empty() {
                        format!("f-{}", folder.position)
                    } else {
                        hex(&folder.identifier)
                    };
                    let map_recipient = |r: presage::proto::Recipient| -> Option<Peer> {
                        match r.identifier {
                            Some(recipient::Identifier::Contact(contact)) => {
                                if super::wire::is_aci(&contact.service_id) {
                                    Some(Peer::Direct(contact.service_id))
                                } else {
                                    aci_bytes(&contact.service_id_binary).map(Peer::Direct)
                                }
                            }
                            Some(recipient::Identifier::GroupMasterKey(raw)) => {
                                <[u8; 32]>::try_from(raw.as_slice()).ok().map(|k| Peer::Group(group_id(&k)))
                            }
                            _ => None,
                        }
                    };
                    folders.push(ChatFolder {
                        id,
                        name: folder.name,
                        position: folder.position,
                        show_only_unread: folder.show_only_unread,
                        show_muted: folder.show_muted_chats,
                        include_all_individual: folder.include_all_individual_chats,
                        include_all_groups: folder.include_all_group_chats,
                        included: folder.included_recipients.into_iter().filter_map(map_recipient).collect(),
                        excluded: folder.excluded_recipients.into_iter().filter_map(map_recipient).collect(),
                    });
                }
                _ => {}
            }
        }
        if !contacts.is_empty() || !groups.is_empty() || account_record.is_some() {
            self.tell(Event::StorageSync(StorageSnapshot {
                contacts,
                groups,
                account: account_record,
            }));
        }
        if !folders.is_empty() {
            folders.sort_by_key(|f| f.position);
            self.tell(Event::Folders(folders));
        }
    }

    fn credentials(&self) -> Result<ServiceCredentials, Fault> {
        let data: &RegistrationData = self.manager.registration_data();
        let password = self.vault.password().ok_or_else(|| Fault::Net("no password kept".into()))?;
        let phone = (&data.phone_number).try_into_e164().map_err(|err| Fault::Net(err.to_string()))?;
        Ok(ServiceCredentials {
            aci: Some(data.service_ids.aci),
            pni: Some(data.service_ids.pni),
            phonenumber: phone,
            password: Some(password),
            device_id: Some(self.manager.device_id()),
        })
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

    /// The groups as stored, told whole. A snapshot remembers what is there and does not announce a change.
    async fn groups_changed(&mut self) {
        let Ok(groups) = self.vault.group_keys() else {
            return;
        };
        let mut told = Vec::new();
        for (key, group) in groups {
            let id = group_id(&key);
            let revision = group.revision;
            let snap = snap_of(&group);
            let timer_secs = snap.timer_secs;
            let member = snap.members.iter().any(|member| member.id == self.me);
            let member_ids = group.members.iter().map(|member| ServiceId::from(member.aci).raw_uuid().to_string()).collect();
            let admins = group
                .members
                .iter()
                .filter(|member| member.role == Role::Administrator)
                .map(|member| ServiceId::from(member.aci).raw_uuid().to_string())
                .collect();
            let member_access = group.access_control.as_ref().map_or(GroupAccess::Member, |ac| access_of(ac.members));
            let attributes_access = group.access_control.as_ref().map_or(GroupAccess::Member, |ac| access_of(ac.attributes));
            let invite_link_access = group.access_control.as_ref().map_or(GroupAccess::Unsatisfiable, |ac| access_of(ac.add_from_invite_link));
            let invite_link = if !group.invite_link_password.is_empty() && invite_link_access != GroupAccess::Unsatisfiable {
                format_group_invite_link(&key, &group.invite_link_password)
            } else {
                String::new()
            };
            self.groups.insert(id.clone(), key);
            self.known.seed(id.clone(), revision, snap);
            told.push(GroupInfo {
                id,
                title: group.title,
                description: group.description.unwrap_or_default(),
                members: group.members.len() as u32,
                member,
                member_ids,
                admins,
                member_access,
                attributes_access,
                invite_link_access,
                announcements_only: false,
                invite_link,
                timer_secs,
            });
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
    /// A newer revision is read from the stored group. The unsigned change on the message is not trusted.
    async fn peer(&mut self, group: Option<&GroupContextV2>, person: &str) -> Peer {
        let key = group.and_then(|group| group.master_key.as_deref()).and_then(|key| <[u8; 32]>::try_from(key).ok());
        match key {
            Some(key) => {
                let id = group_id(&key);
                if !self.known.contains(&id) {
                    self.groups.insert(id.clone(), key);
                    self.groups_changed().await;
                    return Peer::Group(id);
                }
                if let Some(revision) = group.and_then(|group| group.revision) {
                    self.note_group_change(&id, &key, revision).await;
                }
                Peer::Group(id)
            }
            None => Peer::Direct(person.to_owned()),
        }
    }

    /// One body range, with a mention's name already resolved.
    async fn range_of(&mut self, range: &BodyRange) -> Option<Range> {
        let start = range.start.unwrap_or(0);
        let length = range.length.unwrap_or(0);
        match &range.associated_value {
            Some(AssociatedValue::MentionAci(aci)) => Some(self.mention(start, aci).await),
            Some(AssociatedValue::MentionAciBinary(bytes)) => {
                let aci = aci_of(bytes)?;
                Some(self.mention(start, &aci).await)
            }
            Some(AssociatedValue::Style(style)) => mark_of(*style).map(|style| Range::Style { start, length, style }),
            None => None,
        }
    }

    async fn mention(&mut self, start: u32, aci: &str) -> Range {
        let name = if aci == self.me { "you".to_owned() } else { self.name_of(aci).await };
        let label = if name.is_empty() { "@Signal user".to_owned() } else { format!("@{name}") };
        Range::Mention { start, aci: aci.to_owned(), label }
    }

    async fn ranges_of(&mut self, ranges: &[BodyRange]) -> Vec<Range> {
        let mut out = Vec::with_capacity(ranges.len());
        for range in ranges {
            if let Some(range) = self.range_of(range).await {
                out.push(range);
            }
        }
        out
    }

    /// The stored group moved forward. One system line per change, and the row fields the adapter applies.
    async fn note_group_change(&mut self, id: &str, key: &[u8; 32], revision: u32) {
        if !self.known.should_check(id, revision) {
            return;
        }
        // Presage has already fetched this revision into the vault. The message's group_change bytes are not read.
        let Some(group) = self.vault.group_keys().ok().and_then(|groups| groups.into_iter().find(|(stored, _)| stored == key).map(|(_, group)| group)) else {
            return;
        };
        let stored_revision = group.revision;
        let after = snap_of(&group);
        let Some(before) = self.known.advance(id, stored_revision, after.clone()) else {
            return;
        };
        let me = self.me.clone();
        let mut names = HashMap::new();
        for person in people_who_changed(&before, &after) {
            if person == me {
                continue;
            }
            let name = self.name_of(&person).await;
            names.insert(person, name);
        }
        let delta = group_delta(&before, &after, &me, |person| names.get(person).cloned().unwrap_or_default());
        if delta.lines.is_empty() {
            return;
        }
        self.tell(Event::GroupChanged {
            peer: Peer::Group(id.to_owned()),
            by: String::new(),
            changes: delta.lines,
            title: delta.title,
            description: delta.description,
            left: delta.left,
            rejoined: delta.rejoined,
        });
    }

    /// The words of a message: mentions read as names, styles kept, and what is not drawn here named.
    async fn text(&mut self, message: &DataMessage, author: &str, sent: u64) -> Text {
        let body = words(message);
        let ranges = self.ranges_of(&message.body_ranges).await;
        let styled = style::apply(&body, &ranges, &self.me);
        let text = styled.text;
        let mentions_me = styled.mentions_me;
        let spans = styled.spans;
        let unsupported = unsupported_label(message);
        let quote = match message.quote.as_ref() {
            Some(quote) => {
                let qbody = quote.text.clone().unwrap_or_default();
                let qranges = self.ranges_of(&quote.body_ranges).await;
                let quoted = style::apply(&qbody, &qranges, &self.me);
                Some(Quote { sent: quote.id.unwrap_or(0), author: quote.author_aci.clone().unwrap_or_default(), text: quoted.text, spans: quoted.spans })
            }
            None => None,
        };
        let name = if author == self.me { String::new() } else { self.name_of(author).await };
        let contacts = message.contact.iter().map(shared_contact_of).collect();
        Text {
            author: author.to_owned(),
            name,
            sent,
            text,
            quote,
            mentions_me,
            unsupported,
            spans,
            expire_timer: message.expire_timer.unwrap_or(0),
            expire_timer_version: message.expire_timer_version.unwrap_or(0),
            contacts,
        }
    }

    /// One data message, from someone (`outgoing` false) or from us on another device.
    async fn data(&mut self, message: &DataMessage, author: &str, peer: Peer, sent: u64, outgoing: bool) {
        // A poll's data message has an empty body. Read it before the empty-body drop below.
        if let Some(create) = &message.poll_create {
            let name = if author == self.me { String::new() } else { self.name_of(author).await };
            self.tell(Event::Poll {
                peer,
                author: author.to_owned(),
                name,
                sent,
                question: create.question.clone().unwrap_or_default(),
                options: create.options.clone(),
                multiple: create.allow_multiple.unwrap_or(false),
                outgoing,
            });
            return;
        }
        if let Some(vote) = &message.poll_vote {
            let target_author = vote.parse_target_author_aci().map(|aci| ServiceId::from(aci).raw_uuid().to_string()).unwrap_or_default();
            self.tell(Event::PollVote {
                peer,
                by: author.to_owned(),
                author: target_author,
                target: vote.target_sent_timestamp.unwrap_or(0),
                indexes: vote.option_indexes.clone(),
                serial: vote.vote_count.unwrap_or(0),
            });
            return;
        }
        if let Some(end) = &message.poll_terminate {
            self.tell(Event::PollEnded { peer, author: author.to_owned(), target: end.target_sent_timestamp.unwrap_or(0) });
            return;
        }
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
        // DataMessage.Flags.EXPIRATION_TIMER_UPDATE == 2.
        if message.flags.unwrap_or(0) & 2 != 0 {
            self.tell(Event::TimerChanged {
                peer,
                seconds: message.expire_timer.unwrap_or(0),
                version: message.expire_timer_version.unwrap_or(0),
            });
            return;
        }
        // A group change is noticed in `peer`, from the stored group. What remains here with no words is a profile key.
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
                let styled = self.text(&message, &sender, sent).await;
                let at = message.timestamp.unwrap_or(sent);
                self.tell(Event::Edited { peer, author: sender, target: target_sent_timestamp.unwrap_or(0), at, text: styled.text, spans: styled.spans });
            }
            ContentBody::SynchronizeMessage(sync) => self.sync(sync).await,
            ContentBody::ReceiptMessage(ReceiptMessage { r#type, timestamp }) => {
                // A viewed receipt (a voice message or a video played) is read and more.
                let viewed = r#type == Some(receipt_message::Type::Viewed.into());
                let read = viewed || r#type == Some(receipt_message::Type::Read.into());
                if read || r#type == Some(receipt_message::Type::Delivery.into()) {
                    // `sent` is when this receipt left their phone. `timestamp` names our messages.
                    self.tell(Event::Receipt { by: sender, sent: timestamp, read, viewed, at: sent });
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
            ContentBody::CallMessage(message) => self.call_message(message, sender, sent).await,
            _ => {}
        }
    }

    /// A call. The offer rings, and the hangup or busy with its id ends the ring. An offer of
    /// our own (another of our devices announcing a call) is not a call to us. Nothing is sent
    /// back: this client has no call control, so declining is not answering the wire, and the
    /// caller's own no-answer deadline ends the call on their side.
    async fn call_message(&mut self, message: CallMessage, sender: String, sent: u64) {
        if let Some(offer) = message.offer {
            if sender == self.me {
                return;
            }
            let video = offer.r#type == Some(call_message::offer::Type::OfferVideoCall.into());
            let name = self.name_of(&sender).await;
            self.tell(Event::Call { peer: Peer::Direct(sender.clone()), by: sender, name, video, id: offer.id.unwrap_or(0), sent });
            return;
        }
        if let Some(hangup) = message.hangup {
            let answered = hangup.r#type == Some(call_message::hangup::Type::HangupAccepted.into());
            self.tell(Event::CallEnded { id: hangup.id.unwrap_or(0), answered });
            return;
        }
        if let Some(busy) = message.busy {
            self.tell(Event::CallEnded { id: busy.id.unwrap_or(0), answered: false });
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
                let styled = self.text(message, &me, 0).await;
                let at = message.timestamp.or(sent.timestamp).unwrap_or(0);
                self.tell(Event::Edited { peer, author: me, target: target_sent_timestamp.unwrap_or(0), at, text: styled.text, spans: styled.spans });
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
        if let Some(verified_sync) = sync.verified {
            let aci = verified_sync
                .destination_aci
                .filter(|s| super::wire::is_aci(s))
                .or_else(|| verified_sync.destination_aci_binary.as_deref().and_then(aci_bytes));
            if let Some(aci) = aci {
                let state = match verified_sync.state.and_then(|v| verified::State::try_from(v).ok()) {
                    Some(verified::State::Verified) => IdentityState::Verified,
                    Some(verified::State::Unverified) => IdentityState::Unverified,
                    _ => IdentityState::Default,
                };
                let _ = self.vault.set_identity_state(&aci, state, verified_sync.identity_key.as_deref(), true);
                self.tell(Event::VerifiedChanged { aci, state });
            }
        }
        if let Some(delete) = sync.delete_for_me {
            self.deleted_for_me(delete);
        }
        if let Some(blocked) = sync.blocked {
            let mut people = Vec::new();
            for aci in blocked.acis {
                if super::wire::is_aci(&aci) {
                    people.push(aci);
                }
            }
            for raw in blocked.acis_binary {
                if let Some(aci) = aci_bytes(&raw) {
                    if !people.contains(&aci) {
                        people.push(aci);
                    }
                }
            }
            let groups: Vec<String> = blocked.group_ids.into_iter().map(|id| hex(&id)).collect();
            self.blocked_acis = people.iter().cloned().collect();
            self.blocked_groups = groups.iter().cloned().collect();
            self.tell(Event::Blocked { people, groups });
        }
        if let Some(resp) = sync.message_request_response {
            let peer = if let Some(aci) = resp.thread_aci.filter(|s| super::wire::is_aci(s)) {
                Some(Peer::Direct(aci))
            } else if let Some(aci) = resp.thread_aci_binary.as_deref().and_then(aci_bytes) {
                Some(Peer::Direct(aci))
            } else {
                resp.group_id.map(|id| Peer::Group(hex(&id)))
            };
            let action = match resp.r#type.and_then(|v| sync_message::message_request_response::Type::try_from(v).ok()) {
                Some(sync_message::message_request_response::Type::Accept) => Some(MessageRequestAction::Accept),
                Some(sync_message::message_request_response::Type::Delete) => Some(MessageRequestAction::Delete),
                Some(sync_message::message_request_response::Type::Block) => Some(MessageRequestAction::Block),
                Some(sync_message::message_request_response::Type::BlockAndDelete) => Some(MessageRequestAction::BlockAndDelete),
                Some(sync_message::message_request_response::Type::Spam) => Some(MessageRequestAction::Spam),
                Some(sync_message::message_request_response::Type::BlockAndSpam) => Some(MessageRequestAction::BlockAndSpam),
                _ => None,
            };
            if let (Some(peer), Some(action)) = (peer, action) {
                self.tell(Event::MessageRequestResponse { peer, action });
            }
        }
        if let Some(keys) = sync.keys {
            let mut updated_key = false;
            if let Some(aep_str) = keys.account_entropy_pool.as_deref() {
                if let Ok(aep) = AccountEntropyPool::from_str(aep_str) {
                    let _ = self.vault.store_account_entropy_pool(Some(&aep)).await;
                    if let Ok(master) = MasterKey::from_slice(aep.derive_svr_key().as_slice()) {
                        let _ = self.vault.store_master_key(Some(&master)).await;
                    }
                    updated_key = true;
                }
            }
            if let Some(raw_master) = keys.master.as_deref() {
                if let Ok(master) = MasterKey::from_slice(raw_master) {
                    let _ = self.vault.store_master_key(Some(&master)).await;
                    updated_key = true;
                }
            }
            if updated_key {
                self.sync_storage().await;
            }
        }
        if let Some(fetch) = sync.fetch_latest {
            if fetch.r#type == Some(sync_message::fetch_latest::Type::StorageManifest.into()) {
                self.sync_storage().await;
            }
        }
        if let Some(call_event) = sync.call_event {
            self.call_event_sync(call_event);
        }
        if let Some(call_log) = sync.call_log_event {
            self.call_log_sync(call_log);
        }
        if let Some(link_update) = sync.call_link_update {
            if let Some(root_key) = link_update.root_key.and_then(|b| <[u8; 32]>::try_from(b).ok()) {
                self.tell(Event::CallLinkUpdated {
                    root_key,
                    admin_passkey: link_update.admin_passkey,
                });
            }
        }
    }

    fn call_peer_of(bytes: &[u8], is_group: bool) -> Option<Peer> {
        if is_group || bytes.len() == 32 {
            return Some(Peer::Group(hex(bytes)));
        }
        if let Some(aci) = aci_bytes(bytes) {
            return Some(Peer::Direct(aci));
        }
        if bytes.len() == 17 && bytes[0] == 0 {
            if let Some(aci) = aci_bytes(&bytes[1..]) {
                return Some(Peer::Direct(aci));
            }
        }
        if let Ok(text) = std::str::from_utf8(bytes) {
            if super::wire::is_aci(text) {
                return Some(Peer::Direct(text.to_owned()));
            }
        }
        None
    }

    fn call_event_sync(&self, ev: sync_message::CallEvent) {
        let kind = match ev.r#type.and_then(|v| sync_message::call_event::Type::try_from(v).ok()) {
            Some(sync_message::call_event::Type::VideoCall) => CallKind::Video,
            Some(sync_message::call_event::Type::GroupCall) => CallKind::Group,
            Some(sync_message::call_event::Type::AdHocCall) => CallKind::AdHoc,
            _ => CallKind::Voice,
        };
        let Some(conv_bytes) = ev.conversation_id.as_deref() else {
            return;
        };
        let Some(peer) = Self::call_peer_of(conv_bytes, kind == CallKind::Group) else {
            return;
        };
        let incoming = ev.direction != Some(sync_message::call_event::Direction::Outgoing.into());
        let outcome = match ev.event.and_then(|v| sync_message::call_event::Event::try_from(v).ok()) {
            Some(sync_message::call_event::Event::Accepted) => CallOutcome::Accepted,
            Some(sync_message::call_event::Event::NotAccepted) => CallOutcome::NotAccepted,
            Some(sync_message::call_event::Event::Delete) => CallOutcome::Deleted,
            Some(sync_message::call_event::Event::Observed) => CallOutcome::Observed,
            _ => return,
        };
        let call_id = ev.call_id.unwrap_or_else(|| ev.timestamp.unwrap_or_else(now_ms));
        let timestamp = ev.timestamp.unwrap_or_else(now_ms);
        self.tell(Event::CallEvent { peer, call_id, timestamp, kind, incoming, outcome });
    }

    fn call_log_sync(&self, ev: sync_message::CallLogEvent) {
        let kind = match ev.r#type.and_then(|v| sync_message::call_log_event::Type::try_from(v).ok()) {
            Some(sync_message::call_log_event::Type::Clear) => CallLogKind::Clear,
            Some(sync_message::call_log_event::Type::MarkedAsRead) => CallLogKind::MarkedAsRead,
            Some(sync_message::call_log_event::Type::MarkedAsReadInConversation) => CallLogKind::MarkedAsReadInConversation,
            Some(sync_message::call_log_event::Type::ClearInConversation) => CallLogKind::ClearInConversation,
            None => return,
        };
        let peer = ev.conversation_id.as_deref().and_then(|b| Self::call_peer_of(b, false));
        self.tell(Event::CallLogEvent {
            kind,
            peer,
            call_id: ev.call_id,
            timestamp: ev.timestamp.unwrap_or_else(now_ms),
        });
    }

    /// A delete made on another of the user's devices, for this account only.
    fn deleted_for_me(&self, delete: sync_message::DeleteForMe) {
        for batch in delete.message_deletes {
            let Some(peer) = peer_of(batch.conversation) else {
                continue;
            };
            for message in batch.messages {
                if let Some((author, target)) = addressed(&message) {
                    self.tell(Event::DeletedForMe { peer: peer.clone(), author, target });
                }
            }
        }
        for batch in delete.conversation_deletes {
            let Some(peer) = peer_of(batch.conversation) else {
                continue;
            };
            if batch.is_full_delete.unwrap_or(false) {
                self.tell(Event::Forgot(peer));
                continue;
            }
            // A partial delete names the newest messages it covers. Those are the ones removed
            // here; messages older than them are not listed, and this store has no "up to" delete.
            let named = batch.most_recent_messages.into_iter().chain(batch.most_recent_non_expiring_messages);
            for message in named {
                if let Some((author, target)) = addressed(&message) {
                    self.tell(Event::DeletedForMe { peer: peer.clone(), author, target });
                }
            }
        }
        for batch in delete.local_only_conversation_deletes {
            if let Some(peer) = peer_of(batch.conversation) {
                self.tell(Event::Forgot(peer));
            }
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
            Ask::Picture { of, answer } => {
                let _ = answer.send(self.picture(&of).await);
            }
            Ask::Card { answer } => {
                let _ = answer.send(self.card().await);
            }
            Ask::SetCard { name, about, emoji, answer } => {
                let _ = answer.send(self.set_card(name, about, emoji).await);
            }
            Ask::Lookup { username, answer } => {
                let found = match self.manager.lookup_username(&username).await {
                    Ok(aci) => Ok(aci.map(|aci| aci.service_id_string())),
                    Err(err) => Err(fault(err)),
                };
                let _ = answer.send(found);
            }
            Ask::SetMute { answer, .. }
            | Ask::SetArchived { answer, .. }
            | Ask::SetPinned { answer, .. }
            | Ask::SetMarkedUnread { answer, .. } => {
                // Storage-service writes require conflict-free manifest merging (#745); keep local state
                // updated in chatd without risking clobbering the phone's manifest.
                let _ = answer.send(Ok(()));
            }
            Ask::SetBlocked { peer, blocked, answer } => {
                let _ = answer.send(self.set_blocked(&peer, blocked).await);
            }
            Ask::RespondMessageRequest { peer, action, answer } => {
                let _ = answer.send(self.respond_message_request(&peer, action).await);
            }
            Ask::CreateGroup { title, members, answer } => {
                let _ = answer.send(self.create_group(&title, &members).await);
            }
            Ask::LeaveGroup { group_id, answer } => {
                let _ = answer.send(self.leave_group(&group_id).await);
            }
            Ask::GroupMembers { group_id, answer } => {
                let _ = answer.send(self.group_members(&group_id).await);
            }
            Ask::GroupAddMembers { group_id, people, answer } => {
                let _ = answer.send(self.group_add_members(&group_id, &people).await);
            }
            Ask::GroupRemoveMember { group_id, person, answer } => {
                let _ = answer.send(self.group_remove_member(&group_id, &person).await);
            }
            Ask::GroupSetAdmin { group_id, person, admin, answer } => {
                let _ = answer.send(self.group_set_admin(&group_id, &person, admin).await);
            }
            Ask::GroupUpdate { group_id, edit, answer } => {
                let _ = answer.send(self.group_update(&group_id, edit).await);
            }
            Ask::GroupSetAvatar { group_id, bytes, answer } => {
                let _ = answer.send(self.group_set_avatar(&group_id, bytes).await);
            }
            Ask::GroupSetTimer { group_id, seconds, answer } => {
                let _ = answer.send(self.group_set_timer(&group_id, seconds).await);
            }
            Ask::GroupInviteLink { group_id, mode, answer } => {
                let _ = answer.send(self.group_invite_link(&group_id, mode).await);
            }
            Ask::GroupJoinByLink { url, answer } => {
                let _ = answer.send(self.group_join_by_link(&url).await);
            }
            Ask::GroupReviewRequest { group_id, person, approve, answer } => {
                let _ = answer.send(self.group_review_request(&group_id, &person, approve).await);
            }
            Ask::GroupPendingInvite { group_id, accept, answer } => {
                let _ = answer.send(self.group_pending_invite(&group_id, accept).await);
            }
            Ask::CreateCallLink { name, approve_all, answer } => {
                let _ = answer.send(self.create_call_link(&name, approve_all).await);
            }
            Ask::UpdateCallLink { root_key, name, approve_all, answer } => {
                let _ = answer.send(self.update_call_link(root_key, name, approve_all).await);
            }
            Ask::DeleteCallLink { root_key, answer } => {
                let _ = answer.send(self.delete_call_link(root_key).await);
            }
            Ask::JoinCallLink { url, answer } => {
                let _ = answer.send(self.join_call_link(&url).await);
            }
            Ask::SafetyNumber { aci, answer } => {
                let _ = answer.send(Ok(self.vault.safety_number(&self.me, &aci)));
            }
            Ask::SetVerified { aci, state, answer } => {
                let _ = answer.send(self.set_verified(&aci, state).await);
            }
            Ask::PersonProfile { aci, answer } => {
                let _ = answer.send(self.person_profile(&aci).await);
            }
        }
    }

    async fn person_profile(&mut self, aci: &str) -> Result<PersonProfile, Fault> {
        let uuid = Uuid::parse_str(aci).map_err(|_| Fault::Refused("That person isn't one Signal knows.".to_owned()))?;
        let id = ServiceId::Aci(Aci::from(uuid));
        let Some(key) = self.manager.store().profile_key(&id).await.ok().flatten() else {
            return Ok(PersonProfile { key_known: false, ..PersonProfile::default() });
        };
        // Prefer the raw SignalServiceProfile over websocket so we can also read visible badges.
        if let Ok(credentials) = self.credentials() {
            let mut push = PushService::new(SignalServers::Production, Some(credentials.clone()), "tinyclient");
            if let Ok(mut socket) = push.ws::<Identified>("/v1/websocket/", "/v1/keepalive", &[], Some(credentials)).await {
                if let Ok(encrypted) = socket.retrieve_profile_by_id(Aci::from(uuid), Some(key)).await {
                    let badges: Vec<String> = encrypted
                        .badges
                        .iter()
                        .filter(|b| b.visible && !b.name.trim().is_empty())
                        .map(|b| b.name.trim().to_owned())
                        .collect();
                    let cipher = ProfileCipher::new(key);
                    if let Ok(decrypted) = cipher.decrypt(encrypted) {
                        let name = decrypted.name.map(|n| n.to_string()).unwrap_or_default();
                        if !name.is_empty() {
                            self.names.insert(aci.to_owned(), name.clone());
                        }
                        return Ok(PersonProfile {
                            name,
                            about: decrypted.about.unwrap_or_default(),
                            emoji: decrypted.about_emoji.unwrap_or_default(),
                            badges,
                            key_known: true,
                        });
                    }
                }
            }
        }
        let profile = self.manager.retrieve_profile_by_uuid(Aci::from(uuid), key).await.map_err(fault)?;
        let name = profile.name.map(|n| n.to_string()).unwrap_or_default();
        if !name.is_empty() {
            self.names.insert(aci.to_owned(), name.clone());
        }
        Ok(PersonProfile {
            name,
            about: profile.about.unwrap_or_default(),
            emoji: profile.about_emoji.unwrap_or_default(),
            badges: Vec::new(),
            key_known: true,
        })
    }

    async fn set_verified(&mut self, aci: &str, state: IdentityState) -> Result<(), Fault> {
        let uuid = Uuid::parse_str(aci).map_err(|_| Fault::Refused("That person isn't one Signal knows.".to_owned()))?;
        let key = self.vault.peer_identity_key(aci);
        self.vault
            .set_identity_state(aci, state, key.as_deref(), true)
            .map_err(|err| Fault::Net(err.to_string()))?;
        let proto_state = match state {
            IdentityState::Default => verified::State::Default,
            IdentityState::Verified => verified::State::Verified,
            IdentityState::Unverified => verified::State::Unverified,
        };
        let sync = SyncMessage {
            verified: Some(Verified {
                destination_aci: Some(aci.to_owned()),
                destination_aci_binary: Some(uuid.as_bytes().to_vec()),
                identity_key: key,
                state: Some(proto_state.into()),
                null_message: None,
            }),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        let _ = self.sync_to_self(sync).await;
        self.tell(Event::VerifiedChanged { aci: aci.to_owned(), state });
        Ok(())
    }

    async fn create_group(&mut self, title: &str, members: &[String]) -> Result<GroupInfo, Fault> {
        let mut master_key = [0u8; 32];
        getrandom::fill(&mut master_key).map_err(|err| Fault::Net(err.to_string()))?;
        let id = group_id(&master_key);
        let me_aci = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let mut group_members = vec![Member {
            aci: Aci::from(me_aci),
            role: Role::Administrator,
            profile_key: self.manager.registration_data().profile_key(),
            joined_at_version: 0,
            label_emoji: None,
            label: None,
        }];
        let mut member_ids = vec![self.me.clone()];
        for aci_str in members {
            let Ok(uuid) = Uuid::parse_str(aci_str) else {
                continue;
            };
            if member_ids.iter().any(|existing| existing == aci_str) {
                continue;
            }
            let pk = self
                .manager
                .store()
                .profile_key(&ServiceId::Aci(Aci::from(uuid)))
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| ProfileKey::create([0u8; 32]));
            group_members.push(Member {
                aci: Aci::from(uuid),
                role: Role::Default,
                profile_key: pk,
                joined_at_version: 0,
                label_emoji: None,
                label: None,
            });
            member_ids.push(aci_str.clone());
        }
        let group = Group {
            title: title.to_owned(),
            avatar: String::new(),
            disappearing_messages_timer: None,
            access_control: Some(AccessControl {
                attributes: AccessRequired::Member,
                members: AccessRequired::Member,
                add_from_invite_link: AccessRequired::Unsatisfiable,
                member_label: AccessRequired::Member,
            }.into()),
            revision: 0,
            members: group_members.into_iter().map(Into::into).collect(),
            pending_members: Vec::new(),
            requesting_members: Vec::new(),
            invite_link_password: Vec::new(),
            description: None,
        };
        let snap = snap_of(&group);
        self.vault.save_group(master_key, group).await.map_err(|err| Fault::Net(err.to_string()))?;
        self.groups.insert(id.clone(), master_key);
        self.known.seed(id.clone(), 0, snap);
        let at = now_ms();
        let update = ContentBody::DataMessage(DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(master_key.to_vec()),
                revision: Some(0),
                group_change: None,
            }),
            timestamp: Some(at),
            ..Default::default()
        });
        let _ = self.manager.send_message_to_group(&master_key, update, at).await;
        Ok(GroupInfo {
            id,
            title: title.to_owned(),
            description: String::new(),
            members: member_ids.len() as u32,
            member: true,
            member_ids,
            admins: vec![self.me.clone()],
            member_access: GroupAccess::Member,
            attributes_access: GroupAccess::Member,
            invite_link_access: GroupAccess::Unsatisfiable,
            announcements_only: false,
            invite_link: String::new(),
            timer_secs: 0,
        })
    }

    async fn leave_group(&mut self, id: &str) -> Result<(), Fault> {
        let key = *self
            .groups
            .get(id)
            .ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let stored = self.vault.group(key).await.ok().flatten();
        if let Some(group) = &stored {
            let me_admin = group
                .members
                .iter()
                .any(|m| ServiceId::from(m.aci).raw_uuid().to_string() == self.me && m.role == Role::Administrator);
            let other_admin = group
                .members
                .iter()
                .any(|m| ServiceId::from(m.aci).raw_uuid().to_string() != self.me && m.role == Role::Administrator);
            let others_remain = group
                .members
                .iter()
                .any(|m| ServiceId::from(m.aci).raw_uuid().to_string() != self.me);
            if me_admin && !other_admin && others_remain {
                return Err(Fault::Refused("Choose a new admin first.".to_owned()));
            }
        }
        let next_rev = stored.as_ref().map_or(1, |g| g.revision.saturating_add(1));
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let ops = presage::libsignal_service::groups_v2::GroupOperations::new(params);
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, Aci::from(me_uuid)),
            version: next_rev,
            ..Default::default()
        };
        actions
            .delete_members
            .push(ops.build_remove_member_action(Aci::from(me_uuid)).map_err(|err| Fault::Net(err.to_string()))?);
        self.patch_group(id, actions).await?;
        if let Some(mut group) = stored {
            group.members.retain(|m| ServiceId::from(m.aci).raw_uuid().to_string() != self.me);
            group.revision = next_rev;
            let _ = self.vault.save_group(key, group).await;
        }
        self.groups.remove(id);
        Ok(())
    }

    async fn set_blocked(&mut self, peer: &Peer, blocked: bool) -> Result<(), Fault> {
        match peer {
            Peer::Direct(aci) => {
                if blocked {
                    self.blocked_acis.insert(aci.clone());
                } else {
                    self.blocked_acis.remove(aci);
                }
            }
            Peer::Group(id) => {
                if blocked {
                    self.blocked_groups.insert(id.clone());
                } else {
                    self.blocked_groups.remove(id);
                }
            }
        }
        let acis: Vec<String> = self.blocked_acis.iter().cloned().collect();
        let group_ids: Vec<Vec<u8>> = self.blocked_groups.iter().filter_map(|id| unhex(id)).collect();
        let sync = SyncMessage {
            blocked: Some(sync_message::Blocked {
                acis,
                group_ids,
                ..Default::default()
            }),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        self.sync_to_self(sync).await
    }

    async fn respond_message_request(&mut self, peer: &Peer, action: MessageRequestAction) -> Result<(), Fault> {
        let kind = match action {
            MessageRequestAction::Accept => sync_message::message_request_response::Type::Accept,
            MessageRequestAction::Delete => sync_message::message_request_response::Type::Delete,
            MessageRequestAction::Block => sync_message::message_request_response::Type::Block,
            MessageRequestAction::BlockAndDelete => sync_message::message_request_response::Type::BlockAndDelete,
            MessageRequestAction::Spam => sync_message::message_request_response::Type::Spam,
            MessageRequestAction::BlockAndSpam => sync_message::message_request_response::Type::BlockAndSpam,
        };
        let (thread_aci, group_id) = match peer {
            Peer::Direct(aci) => (Some(aci.clone()), None),
            Peer::Group(id) => (None, unhex(id)),
        };
        let sync = SyncMessage {
            message_request_response: Some(sync_message::MessageRequestResponse {
                thread_aci,
                group_id,
                r#type: Some(kind.into()),
                ..Default::default()
            }),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        self.sync_to_self(sync).await?;
        // On Accept for a direct chat, share our profile key with the sender so they see our name.
        if action == MessageRequestAction::Accept {
            if let Peer::Direct(_) = peer {
                let profile_key = self.manager.registration_data().profile_key().get_bytes().to_vec();
                let dm = DataMessage {
                    // DataMessage.Flags.PROFILE_KEY_UPDATE == 4.
                    flags: Some(4),
                    profile_key: Some(profile_key),
                    timestamp: Some(now_ms()),
                    ..Default::default()
                };
                let _ = self.deliver(peer, ContentBody::DataMessage(dm), now_ms()).await;
            }
        }
        Ok(())
    }

    async fn create_call_link(&mut self, name: &str, approve_all: bool) -> Result<CallLink, Fault> {
        let mut root_key = [0u8; 32];
        let mut admin_passkey = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut root_key);
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut admin_passkey);
        let sync = SyncMessage {
            call_link_update: Some(sync_message::CallLinkUpdate {
                root_key: Some(root_key.to_vec()),
                admin_passkey: Some(admin_passkey.to_vec()),
                r#type: Some(sync_message::call_link_update::Type::Update.into()),
            }),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        self.sync_to_self(sync).await?;
        Ok(CallLink {
            root_key,
            url: format_call_link(&root_key),
            name: name.to_owned(),
            approve_all,
            admin_passkey: Some(admin_passkey.to_vec()),
        })
    }

    async fn update_call_link(&mut self, root_key: [u8; 32], name: Option<String>, approve_all: Option<bool>) -> Result<CallLink, Fault> {
        let sync = SyncMessage {
            call_link_update: Some(sync_message::CallLinkUpdate {
                root_key: Some(root_key.to_vec()),
                admin_passkey: None,
                r#type: Some(sync_message::call_link_update::Type::Update.into()),
            }),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        self.sync_to_self(sync).await?;
        Ok(CallLink {
            root_key,
            url: format_call_link(&root_key),
            name: name.unwrap_or_default(),
            approve_all: approve_all.unwrap_or(false),
            admin_passkey: None,
        })
    }

    async fn delete_call_link(&mut self, root_key: [u8; 32]) -> Result<(), Fault> {
        let _ = root_key;
        Ok(())
    }

    async fn join_call_link(&mut self, url: &str) -> Result<CallLinkJoin, Fault> {
        let root_key = parse_call_link(url).ok_or_else(|| Fault::Refused("That is not a valid Signal call link.".into()))?;
        Ok(CallLinkJoin {
            room_id: call_link_room_id(&root_key),
            name: String::new(),
            waiting_for_approval: false,
        })
    }

    async fn group_members(&mut self, group_id: &str) -> Result<GroupMembers, Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let group = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, group)| group))
            .ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let mut members = Vec::with_capacity(group.members.len());
        for m in &group.members {
            let id = m.aci.service_id_string();
            let name = if id == self.me { String::new() } else { self.name_of(&id).await };
            members.push(GroupMember { id, name, admin: m.role == Role::Administrator });
        }
        let mut pending = Vec::with_capacity(group.pending_members.len());
        for p in &group.pending_members {
            let id = p.uuid.to_string();
            let name = self.name_of(&id).await;
            pending.push(GroupMember { id, name, admin: p.role == Role::Administrator });
        }
        let mut requesting = Vec::with_capacity(group.requesting_members.len());
        for r in &group.requesting_members {
            let id = r.aci.service_id_string();
            let name = self.name_of(&id).await;
            requesting.push(GroupMember { id, name, admin: false });
        }
        Ok(GroupMembers { members, pending, requesting })
    }

    /// Patches a group via Signal's Groups v2 API (`PATCH /v1/groups/`), sends the signed
    /// `GroupChange` in a group update message, and refreshes the stored group state.
    async fn patch_group(&mut self, group_id: &str, actions: presage::proto::group_change::Actions) -> Result<(), Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let data: &RegistrationData = self.manager.registration_data();
        let password = self.vault.password().ok_or_else(|| Fault::Net("no password kept".into()))?;
        let phone = (&data.phone_number).try_into_e164().map_err(|err| Fault::Net(err.to_string()))?;
        let credentials = ServiceCredentials {
            aci: Some(data.service_ids.aci),
            pni: Some(data.service_ids.pni),
            phonenumber: phone,
            password: Some(password),
            device_id: Some(self.manager.device_id()),
        };
        let mut push = PushService::new(SignalServers::Production, Some(credentials), "tinyclient");
        let unid_ws = push
            .ws::<presage::libsignal_service::websocket::Unidentified>("/v1/websocket/", "/v1/keepalive", &[], None)
            .await
            .map_err(|err| fault::<vault::VaultError>(err.into()))?;
        let server_public_params = presage::libsignal_service::configuration::ServiceConfiguration::from(SignalServers::Production).zkgroup_server_public_params;
        let mut groups_mgr = presage::libsignal_service::groups_v2::GroupsManager::new(
            data.service_ids.clone(),
            push.clone(),
            unid_ws,
            presage::libsignal_service::groups_v2::InMemoryCredentialsCache::default(),
            server_public_params,
        );
        let group_secret_params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let auth = groups_mgr
            .get_authorization_for_today(&mut rand::rng(), group_secret_params)
            .await
            .map_err(|err| fault::<vault::VaultError>(err.into()))?;
        let encoded_actions = prost::Message::encode_to_vec(&actions);
        let change = presage::proto::GroupChange {
            actions: encoded_actions,
            server_signature: Vec::new(),
            change_epoch: 0,
        };
        let body = prost::Message::encode_to_vec(&change);
        let resp = push
            .request(
                reqwest::Method::PATCH,
                presage::libsignal_service::configuration::Endpoint::storage("/v1/groups/"),
                presage::libsignal_service::push_service::HttpAuthOverride::Identified(auth),
            )
            .map_err(|err| fault::<vault::VaultError>(err.into()))?
            .header("Content-Type", "application/x-protobuf")
            .body(body)
            .send()
            .await
            .map_err(|err| Fault::Net(err.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            // On revision conflict (409), refresh the stored group so local state rolls back cleanly.
            if status == reqwest::StatusCode::CONFLICT {
                if let Ok(encrypted) = groups_mgr.fetch_encrypted_group(&mut rand::rng(), &key).await {
                    if let Ok(decrypted) = presage::libsignal_service::groups_v2::decrypt_group(&key, encrypted) {
                        let _ = self.vault.save_group(key, decrypted).await;
                        self.groups_changed().await;
                    }
                }
            }
            return Err(Fault::Refused(format!("Signal rejected the group change ({status}).")));
        }
        let signed_bytes = resp.bytes().await.map_err(|err| Fault::Net(err.to_string()))?;
        // Refresh the stored group from the server and broadcast the signed change.
        if let Ok(encrypted) = groups_mgr.fetch_encrypted_group(&mut rand::rng(), &key).await {
            if let Ok(decrypted) = presage::libsignal_service::groups_v2::decrypt_group(&key, encrypted) {
                let rev = decrypted.version;
                let _ = self.vault.save_group(key, decrypted).await;
                let ctx = GroupContextV2 {
                    master_key: Some(key.to_vec()),
                    revision: Some(rev),
                    group_change: (!signed_bytes.is_empty()).then(|| signed_bytes.to_vec()),
                };
                let update = ContentBody::DataMessage(DataMessage {
                    group_v2: Some(ctx),
                    timestamp: Some(now_ms()),
                    ..Default::default()
                });
                let _ = self.manager.send_message_to_group(&key, update, now_ms()).await;
            }
        }
        self.groups_changed().await;
        Ok(())
    }

    fn encrypt_aci_bytes(params: &GroupSecretParams, aci: Aci) -> Vec<u8> {
        presage::libsignal_service::zkgroup::serialize(&params.encrypt_service_id(ServiceId::Aci(aci).into()))
    }

    async fn group_add_members(&mut self, group_id: &str, people: &[String]) -> Result<(), Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let revision = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, g)| g.revision))
            .unwrap_or(0);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let ops = presage::libsignal_service::groups_v2::GroupOperations::new(params);
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, Aci::from(me_uuid)),
            version: revision + 1,
            ..Default::default()
        };
        for person in people {
            let uuid = Uuid::parse_str(person).map_err(|_| Fault::Refused(format!("{person} is not a valid Signal account id.")))?;
            let aci = Aci::from(uuid);
            let sid = ServiceId::Aci(aci);
            if let Ok(Some(pk)) = self.manager.store().profile_key(&sid).await {
                let add = ops
                    .build_add_member_action(aci, pk, Role::Default)
                    .map_err(|err| Fault::Net(err.to_string()))?;
                actions.add_members.push(add);
            } else {
                let pending = ops
                    .build_add_pending_member_action(sid, Aci::from(me_uuid), Role::Default)
                    .map_err(|err| Fault::Net(err.to_string()))?;
                actions.add_members_pending_profile_key.push(pending);
            }
        }
        self.patch_group(group_id, actions).await
    }

    async fn group_remove_member(&mut self, group_id: &str, person: &str) -> Result<(), Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let revision = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, g)| g.revision))
            .unwrap_or(0);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let ops = presage::libsignal_service::groups_v2::GroupOperations::new(params);
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let target_uuid = Uuid::parse_str(person).map_err(|_| Fault::Refused("That person can't be removed from here.".into()))?;
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, Aci::from(me_uuid)),
            version: revision + 1,
            ..Default::default()
        };
        actions
            .delete_members
            .push(ops.build_remove_member_action(Aci::from(target_uuid)).map_err(|err| Fault::Net(err.to_string()))?);
        self.patch_group(group_id, actions).await
    }

    async fn group_set_admin(&mut self, group_id: &str, person: &str, admin: bool) -> Result<(), Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let revision = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, g)| g.revision))
            .unwrap_or(0);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let target_uuid = Uuid::parse_str(person).map_err(|_| Fault::Refused("That person's role can't be changed here.".into()))?;
        let role = if admin { Role::Administrator } else { Role::Default };
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, Aci::from(me_uuid)),
            version: revision + 1,
            ..Default::default()
        };
        actions.modify_member_roles.push(presage::proto::group_change::actions::ModifyMemberRoleAction {
            user_id: Self::encrypt_aci_bytes(&params, Aci::from(target_uuid)),
            role: role.into(),
        });
        self.patch_group(group_id, actions).await
    }

    async fn group_update(&mut self, group_id: &str, edit: GroupEdit) -> Result<(), Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let revision = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, g)| g.revision))
            .unwrap_or(0);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let ops = presage::libsignal_service::groups_v2::GroupOperations::new(params);
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let mut rng = rand::rng();
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, Aci::from(me_uuid)),
            version: revision + 1,
            ..Default::default()
        };
        if let Some(title) = edit.title {
            actions.modify_title = Some(presage::proto::group_change::actions::ModifyTitleAction {
                title: ops.encrypt_title(&title, &mut rng),
            });
        }
        if let Some(description) = edit.description {
            let desc_opt = (!description.is_empty()).then_some(description.as_str());
            actions.modify_description = Some(presage::proto::group_change::actions::ModifyDescriptionAction {
                description: ops.encrypt_description(desc_opt, &mut rng),
            });
        }
        if let Some(ma) = edit.member_access {
            actions.modify_member_access = Some(presage::proto::group_change::actions::ModifyMembersAccessControlAction {
                members_access: proto_access(ma).into(),
            });
        }
        if let Some(aa) = edit.attributes_access {
            actions.modify_attributes_access = Some(presage::proto::group_change::actions::ModifyAttributesAccessControlAction {
                attributes_access: proto_access(aa).into(),
            });
        }
        if let Some(ao) = edit.announcements_only {
            actions.modify_announcements_only = Some(presage::proto::group_change::actions::ModifyAnnouncementsOnlyAction {
                announcements_only: ao,
            });
        }
        self.patch_group(group_id, actions).await
    }

    async fn group_set_avatar(&mut self, group_id: &str, bytes: Vec<u8>) -> Result<(), Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let revision = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, g)| g.revision))
            .unwrap_or(0);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let avatar_url = if bytes.is_empty() {
            String::new()
        } else {
            let mut randomness = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rng(), &mut randomness);
            let encrypted = params.encrypt_blob_with_padding(randomness, &bytes, 0);
            let spec = AttachmentSpec {
                content_type: "application/octet-stream".into(),
                length: encrypted.len(),
                ..AttachmentSpec::default()
            };
            let uploaded = self.manager.upload_attachment(spec, encrypted).await.map_err(fault)?;
            let pointer = uploaded.map_err(|err| attachment_refused("group-avatar", &err))?;
            match pointer.attachment_identifier {
                Some(presage::proto::attachment_pointer::AttachmentIdentifier::CdnKey(key)) => key,
                _ => String::new(),
            }
        };
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, Aci::from(me_uuid)),
            version: revision + 1,
            ..Default::default()
        };
        actions.modify_avatar = Some(presage::proto::group_change::actions::ModifyAvatarAction { avatar: avatar_url });
        self.patch_group(group_id, actions).await
    }

    async fn group_set_timer(&mut self, group_id: &str, seconds: u32) -> Result<(), Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let revision = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, g)| g.revision))
            .unwrap_or(0);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let ops = presage::libsignal_service::groups_v2::GroupOperations::new(params);
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let timer = (seconds > 0).then_some(presage::libsignal_service::groups_v2::Timer { duration: seconds });
        let mut rng = rand::rng();
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, Aci::from(me_uuid)),
            version: revision + 1,
            ..Default::default()
        };
        actions.modify_disappearing_message_timer = Some(presage::proto::group_change::actions::ModifyDisappearingMessageTimerAction {
            timer: ops.encrypt_disappearing_messages_timer(timer.as_ref(), &mut rng),
        });
        self.patch_group(group_id, actions).await
    }

    async fn group_invite_link(&mut self, group_id: &str, mode: InviteLinkMode) -> Result<String, Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let existing = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, g)| g));
        let revision = existing.as_ref().map_or(0, |g| g.revision);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, Aci::from(me_uuid)),
            version: revision + 1,
            ..Default::default()
        };
        let mut password = existing.map(|g| g.invite_link_password).unwrap_or_default();
        let need_new_pass = matches!(mode, InviteLinkMode::Reset) || (password.is_empty() && !matches!(mode, InviteLinkMode::Off));
        if need_new_pass {
            let mut buf = [0u8; 16];
            rand::RngCore::fill_bytes(&mut rand::rng(), &mut buf);
            password = buf.to_vec();
            actions.modify_invite_link_password = Some(presage::proto::group_change::actions::ModifyInviteLinkPasswordAction {
                invite_link_password: password.clone(),
            });
        }
        let access = match mode {
            InviteLinkMode::Off => Some(AccessRequired::Unsatisfiable),
            InviteLinkMode::Enabled => Some(AccessRequired::Any),
            InviteLinkMode::ApprovalRequired => Some(AccessRequired::Administrator),
            InviteLinkMode::Reset => None,
        };
        if let Some(access) = access {
            actions.modify_add_from_invite_link_access = Some(presage::proto::group_change::actions::ModifyAddFromInviteLinkAccessControlAction {
                add_from_invite_link_access: access.into(),
            });
        }
        self.patch_group(group_id, actions).await?;
        if matches!(mode, InviteLinkMode::Off) || password.is_empty() {
            Ok(String::new())
        } else {
            Ok(format_group_invite_link(&key, &password))
        }
    }

    async fn group_join_by_link(&mut self, url: &str) -> Result<GroupInfo, Fault> {
        let parsed = parse_group_invite_link(url).ok_or_else(|| Fault::Refused("That is not a valid Signal group link.".into()))?;
        let key = parsed.master_key;
        let id = group_id(&key);
        self.groups.insert(id.clone(), key);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let ops = presage::libsignal_service::groups_v2::GroupOperations::new(params);
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let aci = Aci::from(me_uuid);
        let pk = self.manager.registration_data().profile_key();
        let mut add = ops.build_add_member_action(aci, pk, Role::Default).map_err(|err| Fault::Net(err.to_string()))?;
        add.join_from_invite_link = true;
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, aci),
            version: 0,
            ..Default::default()
        };
        actions.add_members.push(add);
        self.patch_group(&id, actions).await?;
        Ok(GroupInfo {
            id,
            title: "Signal group".into(),
            member: true,
            invite_link: url.to_owned(),
            ..GroupInfo::default()
        })
    }

    async fn group_review_request(&mut self, group_id: &str, person: &str, approve: bool) -> Result<(), Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let revision = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, g)| g.revision))
            .unwrap_or(0);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let target_uuid = Uuid::parse_str(person).map_err(|_| Fault::Refused("That join request can't be reviewed here.".into()))?;
        let enc_target = Self::encrypt_aci_bytes(&params, Aci::from(target_uuid));
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: Self::encrypt_aci_bytes(&params, Aci::from(me_uuid)),
            version: revision + 1,
            ..Default::default()
        };
        if approve {
            actions.promote_members_pending_admin_approval.push(
                presage::proto::group_change::actions::PromoteMemberPendingAdminApprovalAction {
                    user_id: enc_target,
                    role: Role::Default.into(),
                },
            );
        } else {
            actions.delete_members_pending_admin_approval.push(
                presage::proto::group_change::actions::DeleteMemberPendingAdminApprovalAction {
                    deleted_user_id: enc_target,
                },
            );
        }
        self.patch_group(group_id, actions).await
    }

    async fn group_pending_invite(&mut self, group_id: &str, accept: bool) -> Result<(), Fault> {
        let key = *self.groups.get(group_id).ok_or_else(|| Fault::Refused("This group isn't one Signal still knows for you.".to_owned()))?;
        let revision = self
            .vault
            .group_keys()
            .ok()
            .and_then(|groups| groups.into_iter().find(|(stored, _)| *stored == key).map(|(_, g)| g.revision))
            .unwrap_or(0);
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let me_uuid = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        let enc_me = Self::encrypt_aci_bytes(&params, Aci::from(me_uuid));
        let mut actions = presage::proto::group_change::Actions {
            source_user_id: enc_me.clone(),
            version: revision + 1,
            ..Default::default()
        };
        if accept {
            let pk = self.manager.registration_data().profile_key();
            let enc_pk = presage::libsignal_service::zkgroup::serialize(&params.encrypt_profile_key(pk, Aci::from(me_uuid).into()));
            actions.promote_members_pending_profile_key.push(
                presage::proto::group_change::actions::PromoteMemberPendingProfileKeyAction {
                    user_id: enc_me,
                    profile_key: enc_pk,
                    presentation: Vec::new(),
                },
            );
        } else {
            actions.delete_members_pending_profile_key.push(
                presage::proto::group_change::actions::DeleteMemberPendingProfileKeyAction {
                    deleted_user_id: enc_me,
                },
            );
        }
        self.patch_group(group_id, actions).await
    }

    /// Name, about, emoji, and the number this device registered with. A username is not on
    /// the profile this library reads, so the link stays empty.
    async fn card(&mut self) -> Result<OwnCard, Fault> {
        let profile = self.manager.retrieve_profile().await.map_err(fault)?;
        let phone = (&self.manager.registration_data().phone_number).try_into_e164().map(|number| number.to_string()).unwrap_or_default();
        Ok(OwnCard {
            name: profile.name.map(|name| name.to_string()).unwrap_or_default(),
            about: profile.about.unwrap_or_default(),
            emoji: profile.about_emoji.unwrap_or_default(),
            username: String::new(),
            phone,
            link: String::new(),
        })
    }

    /// Encrypts the name, about, and emoji into the versioned profile, tells the other devices
    /// to fetch it, then reads it back. Success is that read matching what was written.
    async fn set_card(&mut self, name: Option<String>, about: Option<String>, emoji: Option<String>) -> Result<(), Fault> {
        let current = self.manager.retrieve_profile().await.map_err(fault)?;
        let profile_name = match name {
            Some(name) if !name.trim().is_empty() => ProfileName { given_name: name, family_name: None },
            Some(_) => return Err(Fault::Refused("A name is needed.".into())),
            None => current.name.clone().filter(|name| !name.is_empty()).unwrap_or(ProfileName { given_name: String::new(), family_name: None }),
        };
        let family_empty = profile_name.family_name.as_ref().is_none_or(|family| family.trim().is_empty());
        if profile_name.given_name.trim().is_empty() && family_empty {
            return Err(Fault::Refused("A name is needed.".into()));
        }
        let about = match about {
            Some(text) if text.is_empty() => None,
            Some(text) => Some(text),
            None => current.about.clone().filter(|text| !text.is_empty()),
        };
        let emoji = match emoji {
            Some(text) if text.is_empty() => None,
            Some(text) => Some(text),
            None => current.about_emoji.clone().filter(|text| !text.is_empty()),
        };
        let expect_name = profile_name.to_string();
        let expect_about = about.clone().unwrap_or_default();
        let expect_emoji = emoji.clone().unwrap_or_default();
        self.manager.update_profile(profile_name, about, emoji).await.map_err(fault)?;
        let sync = SyncMessage {
            fetch_latest: Some(sync_message::FetchLatest { r#type: Some(sync_message::fetch_latest::Type::LocalProfile.into()) }),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        let me = Uuid::parse_str(&self.me).map_err(|_| Fault::Net("no account id".into()))?;
        self.manager.send_message(Aci::from(me), sync, now_ms()).await.map_err(fault)?;
        let read = self.manager.retrieve_profile().await.map_err(fault)?;
        let got_name = read.name.map(|name| name.to_string()).unwrap_or_default();
        let got_about = read.about.unwrap_or_default();
        let got_emoji = read.about_emoji.unwrap_or_default();
        if got_name != expect_name || got_about != expect_about || got_emoji != expect_emoji {
            return Err(Fault::Refused("Signal did not keep that profile. Nothing else was changed.".into()));
        }
        Ok(())
    }

    /// A person's profile picture, fetched from Signal's CDN and opened with their profile key
    /// (which their messages or the phone's contact sync gave us), or a group's, opened with
    /// its master key.
    async fn picture(&mut self, of: &Peer) -> Result<Option<Vec<u8>>, Fault> {
        match of {
            Peer::Direct(aci) => {
                let Ok(uuid) = Uuid::parse_str(aci) else {
                    return Ok(None);
                };
                let Ok(Some(key)) = self.manager.store().profile_key(&ServiceId::Aci(Aci::from(uuid))).await else {
                    return Ok(None);
                };
                self.manager.retrieve_profile_avatar_by_uuid(uuid, key).await.map_err(fault)
            }
            Peer::Group(_) => match self.group_context(of) {
                Ok(Some(context)) => self.manager.retrieve_group_avatar(context).await.map_err(fault),
                Ok(None) | Err(_) => Ok(None),
            },
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
                // The first real error fails the whole send. presage does not say which members were missed.
                self.manager.send_message_to_group(&key, body, at).await.map_err(fault)
            }
        }
    }

    async fn send(&mut self, to: &Peer, body: Body) -> Result<u64, Fault> {
        let at = now_ms();
        let group_v2 = self.group_context(to)?;
        let content = match body {
            Body::Text { text, quote, ranges, timer } => ContentBody::DataMessage(DataMessage {
                body: Some(text),
                body_ranges: proto_ranges(&ranges),
                quote: quoted(quote),
                group_v2,
                expire_timer: (timer.seconds > 0).then_some(timer.seconds),
                expire_timer_version: (timer.version > 0).then_some(timer.version),
                ..Default::default()
            }),
            Body::Files { text, quote, files, timer } => {
                let mut attachments = Vec::with_capacity(files.len());
                for file in files {
                    let name = file.name.clone();
                    let gif = file.gif || file.mime == "image/gif";
                    let thumb = file.thumb;
                    let spec = AttachmentSpec {
                        content_type: file.mime,
                        length: file.bytes.len(),
                        file_name: Some(name.clone()),
                        width: (file.width > 0).then_some(file.width),
                        height: (file.height > 0).then_some(file.height),
                        blur_hash: file.blurhash,
                        ..AttachmentSpec::default()
                    };
                    let uploaded = self.manager.upload_attachment(spec, file.bytes).await.map_err(fault)?;
                    let mut pointer = uploaded.map_err(|err| attachment_refused(&name, &err))?;
                    if !thumb.is_empty() {
                        pointer.thumbnail = Some(thumb);
                    }
                    if gif {
                        // SignalService.proto AttachmentPointer.Flags.GIF.
                        pointer.flags = Some(pointer.flags.unwrap_or(0) | 8);
                    }
                    attachments.push(pointer);
                }
                ContentBody::DataMessage(DataMessage {
                    body: (!text.is_empty()).then_some(text),
                    attachments,
                    quote: quoted(quote),
                    timestamp: Some(at),
                    group_v2,
                    expire_timer: (timer.seconds > 0).then_some(timer.seconds),
                    expire_timer_version: (timer.version > 0).then_some(timer.version),
                    ..Default::default()
                })
            }
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
            Body::DeleteForMe { author, target } => {
                let delete_for_me = sync_message::DeleteForMe {
                    message_deletes: vec![sync_message::delete_for_me::MessageDeletes {
                        conversation: Some(conversation_of(to)?),
                        messages: vec![addressable(&author, target)],
                    }],
                    ..Default::default()
                };
                let sync = SyncMessage { delete_for_me: Some(delete_for_me), ..SyncMessage::with_padding(&mut rand::rng()) };
                self.sync_to_self(sync).await?;
                return Ok(at);
            }
            Body::Edit { target, text, ranges, timer } => ContentBody::EditMessage(EditMessage {
                target_sent_timestamp: Some(target),
                data_message: Some(DataMessage {
                    body: Some(text),
                    body_ranges: proto_ranges(&ranges),
                    timestamp: Some(at),
                    group_v2,
                    expire_timer: (timer.seconds > 0).then_some(timer.seconds),
                    expire_timer_version: (timer.version > 0).then_some(timer.version),
                    ..Default::default()
                }),
            }),
            Body::Poll { question, options, multiple, timer } => ContentBody::DataMessage(DataMessage {
                poll_create: Some(data_message::PollCreate { question: Some(question), allow_multiple: Some(multiple), options }),
                timestamp: Some(at),
                required_protocol_version: Some(8),
                group_v2,
                expire_timer: (timer.seconds > 0).then_some(timer.seconds),
                expire_timer_version: (timer.version > 0).then_some(timer.version),
                ..Default::default()
            }),
            Body::PollVote { author, target, indexes, serial } => {
                let uuid = Uuid::parse_str(&author).map_err(|_| Fault::Refused("That person can't be messaged from here.".to_owned()))?;
                ContentBody::DataMessage(DataMessage {
                    poll_vote: Some(data_message::PollVote {
                        target_author_aci_binary: Some(uuid.as_bytes().to_vec()),
                        target_sent_timestamp: Some(target),
                        option_indexes: indexes,
                        vote_count: Some(serial),
                    }),
                    timestamp: Some(at),
                    group_v2,
                    ..Default::default()
                })
            }
            Body::PollEnd { target } => ContentBody::DataMessage(DataMessage {
                poll_terminate: Some(data_message::PollTerminate { target_sent_timestamp: Some(target) }),
                timestamp: Some(at),
                group_v2,
                ..Default::default()
            }),
            Body::Contact { contact, timer } => ContentBody::DataMessage(DataMessage {
                contact: vec![proto_contact(&contact)],
                timestamp: Some(at),
                group_v2,
                expire_timer: (timer.seconds > 0).then_some(timer.seconds),
                expire_timer_version: (timer.version > 0).then_some(timer.version),
                ..Default::default()
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
        self.sync_to_self(sync).await
    }

    /// A sync for the user's other devices, sent to this account.
    async fn sync_to_self(&mut self, sync: SyncMessage) -> Result<(), Fault> {
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

fn span_ranges(text: &str, spans: &[protocol::Span]) -> Vec<Range> {
    style::marks(text, spans).into_iter().map(|(start, length, style)| Range::Style { start, length, style }).collect()
}

fn proto_ranges(ranges: &[Range]) -> Vec<BodyRange> {
    ranges
        .iter()
        .filter_map(|range| match range {
            Range::Style { start, length, style } => {
                let style = match style {
                    Mark::Bold => RangeStyle::Bold,
                    Mark::Italic => RangeStyle::Italic,
                    Mark::Strike => RangeStyle::Strikethrough,
                    Mark::Monospace => RangeStyle::Monospace,
                    Mark::Spoiler => RangeStyle::Spoiler,
                };
                Some(BodyRange {
                    start: Some(*start),
                    length: Some(*length),
                    associated_value: Some(AssociatedValue::Style(style as i32)),
                })
            }
            Range::Mention { start, aci, .. } => Some(BodyRange {
                start: Some(*start),
                length: Some(1),
                associated_value: Some(AssociatedValue::MentionAci(aci.clone())),
            }),
        })
        .collect()
}

fn mark_of(style: i32) -> Option<Mark> {
    match RangeStyle::try_from(style) {
        Ok(RangeStyle::Bold) => Some(Mark::Bold),
        Ok(RangeStyle::Italic) => Some(Mark::Italic),
        Ok(RangeStyle::Spoiler) => Some(Mark::Spoiler),
        Ok(RangeStyle::Strikethrough) => Some(Mark::Strike),
        Ok(RangeStyle::Monospace) => Some(Mark::Monospace),
        _ => None,
    }
}

fn aci_of(bytes: &[u8]) -> Option<String> {
    let bytes: [u8; 16] = bytes.try_into().ok()?;
    Some(Uuid::from_bytes(bytes).to_string())
}

fn shared_contact_of(contact: &data_message::Contact) -> protocol::SharedContact {
    let name = contact
        .name
        .as_ref()
        .map(|n| {
            let parts: Vec<&str> = [
                n.prefix.as_deref(),
                n.given_name.as_deref(),
                n.middle_name.as_deref(),
                n.family_name.as_deref(),
                n.suffix.as_deref(),
            ]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
            if !parts.is_empty() {
                parts.join(" ")
            } else {
                n.nickname.as_deref().map(str::trim).unwrap_or("").to_owned()
            }
        })
        .unwrap_or_default();
    let phones = contact
        .number
        .iter()
        .filter_map(|p| p.value.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
        .collect();
    let emails = contact
        .email
        .iter()
        .filter_map(|e| e.value.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
        .collect();
    let organization = contact.organization.as_deref().map(str::trim).unwrap_or("").to_owned();
    protocol::SharedContact { name, phones, emails, organization, avatar: None }
}

fn proto_contact(contact: &protocol::SharedContact) -> data_message::Contact {
    data_message::Contact {
        name: (!contact.name.is_empty()).then(|| data_message::contact::Name {
            given_name: Some(contact.name.clone()),
            ..Default::default()
        }),
        number: contact
            .phones
            .iter()
            .map(|value| data_message::contact::Phone { value: Some(value.clone()), ..Default::default() })
            .collect(),
        email: contact
            .emails
            .iter()
            .map(|value| data_message::contact::Email { value: Some(value.clone()), ..Default::default() })
            .collect(),
        organization: (!contact.organization.is_empty()).then(|| contact.organization.clone()),
        ..Default::default()
    }
}

fn quoted(quote: Option<super::wire::Quote>) -> Option<data_message::Quote> {
    quote.map(|quote| data_message::Quote {
        id: Some(quote.sent),
        author_aci: Some(quote.author),
        body_ranges: proto_ranges(&span_ranges(&quote.text, &quote.spans)),
        text: Some(quote.text),
        r#type: Some(data_message::quote::Type::Normal.into()),
        ..Default::default()
    })
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
            let mut session = Session { manager, vault: store.clone(), me: aci, events: channel().0, names: HashMap::new(), groups: HashMap::new(), known: GroupRevisions::default(), blocked_acis: HashSet::new(), blocked_groups: HashSet::new() };
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

/// The lab's signal-payment-labels driver: the words of a payment and a gift badge, read off the
/// proto the way the wire carries them. A build without this stays up, so the driver fails.
pub fn prove_payment_labels() -> Result<(), String> {
    let mut message = DataMessage::default();
    message.payment = Some(data_message::Payment {
        item: Some(data_message::payment::Item::Notification(data_message::payment::Notification {
            note: Some("for coffee".to_owned()),
            transaction: None,
        })),
    });
    let payment = message.payment.as_ref().ok_or("the payment did not stay set")?;
    let said = payment_label(payment_note(payment));
    if said != "Payment · open Signal on your phone — for coffee" {
        return Err(format!("a payment with a note is labelled {said:?}"));
    }

    let mut message = DataMessage::default();
    message.payment = Some(data_message::Payment {
        item: Some(data_message::payment::Item::Notification(data_message::payment::Notification {
            note: None,
            transaction: None,
        })),
    });
    let payment = message.payment.as_ref().ok_or("the bare payment did not stay set")?;
    let said = payment_label(payment_note(payment));
    if said != "Payment · open Signal on your phone" {
        return Err(format!("a payment without a note is labelled {said:?}"));
    }

    let mut message = DataMessage::default();
    message.gift_badge = Some(data_message::GiftBadge { receipt_credential_presentation: Some(vec![0xde, 0xad]) });
    if message.gift_badge.is_none() {
        return Err("the gift badge did not stay set".into());
    }
    let said = gift_badge_label();
    if said != "Gift badge · open Signal on your phone to redeem" {
        return Err(format!("a gift badge is labelled {said:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payment as the wire carries it: the sender's note beside a receipt no wallet here opens.
    fn payment(note: Option<&str>) -> DataMessage {
        let mut message = DataMessage::default();
        message.payment = Some(data_message::Payment {
            item: Some(data_message::payment::Item::Notification(data_message::payment::Notification {
                note: note.map(str::to_owned),
                transaction: Some(data_message::payment::notification::Transaction::MobileCoin(
                    data_message::payment::notification::MobileCoin { receipt: Some(vec![0xde, 0xad]) },
                )),
            })),
        });
        message
    }

    fn gift_badge() -> DataMessage {
        let mut message = DataMessage::default();
        message.gift_badge = Some(data_message::GiftBadge { receipt_credential_presentation: Some(vec![0xbe, 0xef]) });
        message
    }

    #[test]
    fn a_payment_names_where_it_is_acting_and_shows_its_note() {
        // The note is the one readable field; the amount stays unread in its encrypted receipt.
        let with_note = payment(Some("for coffee"));
        let paid = with_note.payment.as_ref().unwrap();
        assert_eq!(payment_label(payment_note(paid)), "Payment · open Signal on your phone — for coffee");

        // No note, no dash: the words stand alone.
        let bare = payment(None);
        let paid = bare.payment.as_ref().unwrap();
        assert_eq!(payment_label(payment_note(paid)), "Payment · open Signal on your phone");

        // A payments-activation request is a payment too, and carries no note.
        let mut activation = DataMessage::default();
        activation.payment = Some(data_message::Payment {
            item: Some(data_message::payment::Item::Activation(data_message::payment::Activation {
                r#type: Some(data_message::payment::activation::Type::Request as i32),
            })),
        });
        let paid = activation.payment.as_ref().unwrap();
        assert_eq!(payment_label(payment_note(paid)), "Payment · open Signal on your phone");
    }

    #[test]
    fn a_gift_badge_says_where_it_is_redeemed() {
        // The presentation is opaque bytes here; redeeming is the phone's work and the words say so.
        let badge = gift_badge();
        assert!(badge.gift_badge.is_some());
        assert_eq!(gift_badge_label(), "Gift badge · open Signal on your phone to redeem");
    }

    /// A view-once message as the wire carries it: the flag, usually one attachment with its
    /// content type, and no caption.
    fn view_once(content_type: Option<&str>) -> DataMessage {
        let mut message = DataMessage::default();
        message.is_view_once = Some(true);
        if let Some(kind) = content_type {
            message.attachments.push(presage::proto::AttachmentPointer { content_type: Some(kind.to_owned()), ..Default::default() });
        }
        message
    }

    fn photo() -> DataMessage {
        let mut message = DataMessage::default();
        message.attachments.push(presage::proto::AttachmentPointer { content_type: Some("image/jpeg".to_owned()), ..Default::default() });
        message
    }

    #[test]
    fn a_view_once_photo_is_not_promised_as_a_photo() {
        // The attachment branch used to name it "Photo": a row that opens to nothing and tells
        // nobody why.
        assert_eq!(unsupported_label(&view_once(Some("image/jpeg"))).as_deref(), Some("View-once photo · open it on your phone"));
        assert_eq!(unsupported_label(&view_once(Some("video/mp4"))).as_deref(), Some("View-once video · open it on your phone"));
        assert_eq!(unsupported_label(&view_once(None)).as_deref(), Some("View-once media · open it on your phone"));

        // Without the flag a photo stays a photo, and an explicit false is no view-once.
        assert_eq!(unsupported_label(&photo()).as_deref(), Some("Photo"));
        let mut plain = photo();
        plain.is_view_once = Some(false);
        assert_eq!(unsupported_label(&plain).as_deref(), Some("Photo"));
    }

    /// A message whose text is too long for one message: 2 KB in the body, the whole text as a
    /// plain-text pointer.
    fn long() -> DataMessage {
        let mut message = DataMessage::default();
        message.body = Some("x".repeat(2_048));
        message.attachments.push(Default::default());
        if let Some(attachment) = message.attachments.last_mut() {
            attachment.content_type = Some(LONG_TEXT.to_owned());
        }
        message
    }

    fn with_attachment(kind: &str) -> DataMessage {
        let mut message = DataMessage::default();
        message.attachments.push(Default::default());
        if let Some(attachment) = message.attachments.last_mut() {
            attachment.content_type = Some(kind.to_owned());
        }
        message
    }

    #[test]
    fn a_long_text_is_the_message_not_a_file() {
        let body = "x".repeat(2_048);
        assert_eq!(unsupported_label(&long()), None);
        assert_eq!(words(&long()), format!("{body}{CONTINUES}"));
        // A file of no kind the window draws keeps its name, and so does everything else.
        assert_eq!(unsupported_label(&with_attachment("application/pdf")).as_deref(), Some("File"));
        assert_eq!(unsupported_label(&with_attachment("image/jpeg")).as_deref(), Some("Photo"));
        assert_eq!(unsupported_label(&with_attachment("video/mp4")).as_deref(), Some("Video"));
        assert_eq!(unsupported_label(&with_attachment("audio/ogg")).as_deref(), Some("Audio"));
        let mut sticker = DataMessage::default();
        sticker.sticker = Some(Default::default());
        assert_eq!(unsupported_label(&sticker).as_deref(), Some("Sticker"));
        assert_eq!(unsupported_label(&DataMessage::default()), None);
        assert_eq!(words(&DataMessage::default()), String::new());
    }
}
