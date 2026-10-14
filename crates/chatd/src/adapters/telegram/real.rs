//! The wire to Telegram itself: grammers on one thread of its own.
//!
//! grammers is async. Everything of it runs on a current-thread tokio runtime that this file
//! builds, owns and ends; no other part of chatd sees a future. The adapter's own threads are
//! plain ones and talk to it over channels: a request goes in with a channel for its answer,
//! events come out in order.
//!
//! grammers keeps a session behind a trait. Its stock storage is a second SQLite (libsql), which
//! is left out of the build: [`Vault`] holds the same things in memory, and what must outlive
//! chatd (the keys, the home datacenter) is written to the keychain by the adapter as one small
//! JSON item (`Saved`), like every other network's session.

use super::wire::{Event, Fault, Wire};
use super::{Saved, SavedDc};
use grammers_client::client::{ClientConfiguration, NoRetries, UpdatesConfiguration};
use grammers_client::peer::{Peer, PeerMap};
use grammers_client::Client;
use grammers_mtsender::{ConnectionParams, InvocationError, SenderPool};
use grammers_session::types::{ChannelState, DcOption, PeerAuth, PeerId, PeerInfo, UpdateState, UpdatesState};
use grammers_session::updates::UpdatesLike;
use grammers_session::{BoxFuture, Session, SessionData};
use grammers_tl_types as tl;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// Peers the library is told of and may ask back for (to fill a gap in a channel's updates).
/// Past this the cache starts over; what it then lacks is learned again from the next answer
/// that names it.
const PEERS_KEPT: usize = 20_000;
/// Updates waiting for the adapter. Past this the library drops the newest and says so.
const UPDATES_QUEUED: usize = 2_000;

#[derive(Debug)]
pub struct Poisoned;

impl std::fmt::Display for Poisoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the session lock is poisoned")
    }
}

impl std::error::Error for Poisoned {}

/// The session as the library sees it. In memory only; `saved` is what goes to the keychain.
pub struct Vault {
    data: Mutex<SessionData>,
    me: Mutex<Option<PeerInfo>>,
    /// A key was made, or the home moved, since the session was last written out.
    unsaved: AtomicBool,
}

impl Vault {
    fn new(saved: Option<&Saved>) -> Vault {
        let mut data = SessionData::default();
        let mut me = None;
        if let Some(saved) = saved {
            data.home_dc = saved.home;
            for dc in &saved.dcs {
                let key = unhex(&dc.key).and_then(|key| <[u8; 256]>::try_from(key).ok());
                let known = data.dc_options.get(&dc.id).cloned();
                let (v4, v6) = (dc.v4.parse().ok(), dc.v6.parse().ok());
                let option = match (known, v4, v6) {
                    (Some(known), v4, v6) => DcOption {
                        id: dc.id,
                        ipv4: v4.unwrap_or(known.ipv4),
                        ipv6: v6.unwrap_or(known.ipv6),
                        auth_key: key,
                    },
                    (None, Some(ipv4), Some(ipv6)) => DcOption { id: dc.id, ipv4, ipv6, auth_key: key },
                    _ => continue,
                };
                data.dc_options.insert(dc.id, option);
            }
            me = Some(PeerInfo::User {
                id: saved.user,
                auth: None,
                bot: Some(false),
                is_self: Some(true),
            });
        }
        Vault {
            data: Mutex::new(data),
            me: Mutex::new(me),
            unsaved: AtomicBool::new(false),
        }
    }

    fn data(&self) -> Result<std::sync::MutexGuard<'_, SessionData>, Poisoned> {
        self.data.lock().map_err(|_| Poisoned)
    }

    fn saved(&self, user: i64, name: &str) -> Saved {
        let data = self.data.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut dcs: Vec<SavedDc> = data
            .dc_options
            .values()
            .filter_map(|dc| {
                Some(SavedDc {
                    id: dc.id,
                    v4: dc.ipv4.to_string(),
                    v6: dc.ipv6.to_string(),
                    key: hex(dc.auth_key.as_ref()?),
                })
            })
            .collect();
        dcs.sort_by_key(|dc| dc.id);
        Saved {
            user,
            name: name.to_owned(),
            home: data.home_dc,
            dcs,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok()).collect()
}

impl Session for Vault {
    type Error = Poisoned;

    fn home_dc_id(&self) -> Result<i32, Poisoned> {
        Ok(self.data()?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Poisoned>> {
        Box::pin(async move {
            self.data()?.home_dc = dc_id;
            self.unsaved.store(true, Ordering::Relaxed);
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Poisoned> {
        Ok(self.data()?.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Poisoned>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            let before = self.data()?.dc_options.insert(dc_option.id, dc_option.clone());
            if before.as_ref() != Some(&dc_option) {
                self.unsaved.store(true, Ordering::Relaxed);
            }
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Poisoned>> {
        Box::pin(async move {
            let me = self.me.lock().map_err(|_| Poisoned)?.clone();
            if peer == PeerId::self_user() || me.as_ref().is_some_and(|me| me.id() == peer) {
                return Ok(me);
            }
            Ok(self.data()?.peer_infos.get(&peer).cloned())
        })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), Poisoned>> {
        let peer = peer.clone();
        Box::pin(async move {
            if matches!(peer, PeerInfo::User { is_self: Some(true), .. }) {
                *self.me.lock().map_err(|_| Poisoned)? = Some(peer);
                return Ok(());
            }
            let mut data = self.data()?;
            if data.peer_infos.len() >= PEERS_KEPT {
                data.peer_infos.clear();
            }
            data.peer_infos.entry(peer.id()).or_insert_with(|| peer.clone()).extend_info(&peer);
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Poisoned>> {
        Box::pin(async move { Ok(self.data()?.updates_state.clone()) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Poisoned>> {
        Box::pin(async move {
            let mut data = self.data()?;
            match update {
                UpdateState::All(state) => data.updates_state = state,
                UpdateState::Primary { pts, date, seq } => {
                    data.updates_state.pts = pts;
                    data.updates_state.date = date;
                    data.updates_state.seq = seq;
                }
                UpdateState::Secondary { qts } => data.updates_state.qts = qts,
                UpdateState::Channel { id, pts } => {
                    data.updates_state.channels.retain(|channel| channel.id != id);
                    data.updates_state.channels.push(ChannelState { id, pts });
                }
            }
            Ok(())
        })
    }
}

struct Job {
    dc: Option<i32>,
    body: Vec<u8>,
    reply: Sender<Result<Vec<u8>, Fault>>,
}

pub struct Real {
    /// Dropping this ends the runtime's loop, and with it the thread and the connection.
    jobs: Mutex<Option<UnboundedSender<Job>>>,
    events: Mutex<Option<Receiver<Event>>>,
    vault: Arc<Vault>,
}

impl Real {
    /// Starts the thread and its runtime. Nothing is sent until the first request.
    pub fn open(api_id: i32, saved: Option<&Saved>) -> Result<Real, String> {
        let vault = Arc::new(Vault::new(saved));
        let (jobs, inbox) = unbounded_channel();
        let (events, heard) = channel();
        let held = vault.clone();
        std::thread::Builder::new()
            .name("telegram".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
                match runtime {
                    Ok(runtime) => runtime.block_on(run(api_id, held, inbox, events)),
                    Err(err) => drop(events.send(Event::Closed(format!("no runtime: {err}")))),
                }
            })
            .map_err(|err| err.to_string())?;
        Ok(Real {
            jobs: Mutex::new(Some(jobs)),
            events: Mutex::new(Some(heard)),
            vault,
        })
    }
}

fn fault(err: InvocationError) -> Fault {
    match err {
        InvocationError::Rpc(rpc) => Fault::Rpc {
            code: rpc.code,
            name: rpc.name,
            value: rpc.value,
        },
        other => Fault::Net(other.to_string()),
    }
}

/// The people and chats an update's container named, as Telegram sent them.
fn named(peers: &PeerMap) -> (Vec<tl::enums::User>, Vec<tl::enums::Chat>) {
    let (mut users, mut chats) = (Vec::new(), Vec::new());
    for peer in peers.iter_peers() {
        match peer {
            Peer::User(user) => users.push(user.raw.clone()),
            Peer::Group(group) => chats.push(group.raw.clone()),
            Peer::Channel(channel) => chats.push(tl::enums::Chat::Channel(channel.raw.clone())),
        }
    }
    (users, chats)
}

/// Follows the account's updates, in order and with gaps filled by the library, until the
/// connection is lost for good.
async fn follow(client: Client, updates: UnboundedReceiver<UpdatesLike>, events: Sender<Event>) {
    let configuration = UpdatesConfiguration {
        catch_up: false,
        update_queue_limit: Some(UPDATES_QUEUED),
    };
    let mut stream = match client.stream_updates(updates, configuration).await {
        Ok(stream) => stream,
        Err(err) => return drop(events.send(Event::Closed(err.to_string()))),
    };
    loop {
        match stream.next_raw().await {
            Ok((update, _, peers)) => {
                let (users, chats) = named(&peers);
                if events.send(Event::Update { update: Box::new(update), users, chats }).is_err() {
                    return;
                }
            }
            Err(err) => return drop(events.send(Event::Closed(err.to_string()))),
        }
    }
}

async fn run(api_id: i32, vault: Arc<Vault>, mut jobs: UnboundedReceiver<Job>, events: Sender<Event>) {
    // What the account's device list shows for this session.
    let params = ConnectionParams {
        device_model: "Mac".to_owned(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        ..ConnectionParams::default()
    };
    let SenderPool { runner, handle, updates } = SenderPool::with_configuration(vault.clone(), api_id, params);
    // Flood waits are the adapter's to honor (`wire::patient`), so the library retries nothing.
    let configuration = ClientConfiguration {
        retry_policy: Box::new(NoRetries),
        auto_cache_peers: true,
    };
    let client = Client::with_configuration(handle.clone(), configuration);
    let pool = tokio::spawn(runner.run());
    let following = tokio::spawn(follow(client, updates, events));
    while let Some(job) = jobs.recv().await {
        let Ok(home) = vault.home_dc_id() else {
            break;
        };
        let thin = handle.thin.clone();
        tokio::spawn(async move {
            let answer = thin.invoke_in_dc(job.dc.unwrap_or(home), job.body).await;
            let _ = job.reply.send(answer.map_err(fault));
        });
    }
    following.abort();
    handle.thin.quit();
    let _ = pool.await;
}

impl Wire for Real {
    fn invoke(&self, dc: Option<i32>, body: Vec<u8>) -> Result<Vec<u8>, Fault> {
        let (reply, answer) = channel();
        let closed = || Fault::Net("the connection is closed".to_owned());
        match self.jobs.lock().map_err(|_| closed())?.as_ref() {
            Some(jobs) => jobs.send(Job { dc, body, reply }).map_err(|_| closed())?,
            None => return Err(closed()),
        }
        // The library answers every request, with an error once its connection is gone.
        answer.recv().unwrap_or_else(|_| Err(Fault::Net("the connection is closed".to_owned())))
    }

    fn events(&self) -> Option<Receiver<Event>> {
        self.events.lock().ok()?.take()
    }

    fn home(&self) -> i32 {
        self.vault.home_dc_id().unwrap_or(2)
    }

    fn set_home(&self, dc: i32) {
        if let Ok(mut data) = self.vault.data() {
            data.home_dc = dc;
        }
        self.vault.unsaved.store(true, Ordering::Relaxed);
    }

    fn signed_in(&self, user: &tl::types::User) {
        let me = PeerInfo::User {
            id: user.id,
            auth: user.access_hash.map(PeerAuth::from_hash),
            bot: Some(user.bot),
            is_self: Some(true),
        };
        if let Ok(mut held) = self.vault.me.lock() {
            *held = Some(me);
        }
    }

    fn session(&self, user: i64, name: &str) -> String {
        self.vault.unsaved.store(false, Ordering::Relaxed);
        serde_json::to_string(&self.vault.saved(user, name)).unwrap_or_default()
    }

    fn unsaved(&self) -> bool {
        self.vault.unsaved.swap(false, Ordering::Relaxed)
    }

    fn close(&self) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_comes_back_with_its_keys_and_home() {
        let key = "ab".repeat(256);
        let saved = Saved {
            user: 7001,
            name: "Mira".into(),
            home: 4,
            dcs: vec![SavedDc { id: 4, v4: "149.154.167.92:443".into(), v6: "[2001:67c:4e8:f004::a]:443".into(), key: key.clone() }],
        };
        let vault = Vault::new(Some(&saved));
        assert_eq!(vault.home_dc_id().unwrap(), 4);
        assert_eq!(vault.dc_option(4).unwrap().unwrap().auth_key, Some([0xab; 256]));
        // The other datacenters are known by address and have no key yet.
        assert_eq!(vault.dc_option(2).unwrap().unwrap().auth_key, None);
        let back = vault.saved(7001, "Mira");
        assert_eq!((back.home, back.dcs.len(), back.dcs[0].key.as_str()), (4, 1, key.as_str()));
        assert_eq!(unhex("zz"), None);
    }
}
