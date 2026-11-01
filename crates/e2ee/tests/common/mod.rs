//! A fake delivery service and a set of devices, to run the real MLS flow end to end: the
//! server signs external proposals, any member commits, the server orders and relays.
#![allow(dead_code)]

use e2ee::engine::Engine;
use e2ee::envelope::{Envelope, Kind};
use e2ee::groups::{CommitBundle, CommitOutcome};
use e2ee::identity::{DeviceKey, DeviceRecord};
use e2ee::{Claimed, ConversationId, DeviceId, Incoming, UserId};
use openmls::prelude::tls_codec::Serialize as _;
use openmls::prelude::{ExternalProposal, GroupEpoch, GroupId, KeyPackageIn, LeafNodeIndex, MlsMessageOut, ProtocolVersion, SenderExtensionIndex};
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider as _;
use std::collections::BTreeMap;

pub struct Ds {
    pub key: DeviceKey,
    pub conv: ConversationId,
}

impl Ds {
    pub fn new() -> Ds {
        Ds { key: DeviceKey::generate().unwrap(), conv: ConversationId::random().unwrap() }
    }

    pub fn public(&self) -> Vec<u8> {
        self.key.public_key()
    }

    /// A proposals message (a TLS vector of external proposals, removes first as the server does).
    pub fn proposals(&self, adds: &[&[u8]], removes: &[u32], epoch: u64) -> Vec<u8> {
        self.proposals_signed_by(&self.key, adds, removes, epoch)
    }

    pub fn proposals_signed_by(&self, signer: &DeviceKey, adds: &[&[u8]], removes: &[u32], epoch: u64) -> Vec<u8> {
        let crypto = OpenMlsRustCrypto::default();
        let group = GroupId::from_slice(&self.conv.0);
        let mut messages: Vec<MlsMessageOut> = Vec::new();
        for leaf in removes {
            messages.push(
                ExternalProposal::new_remove::<OpenMlsRustCrypto>(LeafNodeIndex::new(*leaf), group.clone(), GroupEpoch::from(epoch), signer, SenderExtensionIndex::new(0)).unwrap(),
            );
        }
        for package in adds {
            let parsed = KeyPackageIn::tls_deserialize_exact_bytes(package).unwrap().validate(crypto.crypto(), ProtocolVersion::Mls10).unwrap();
            messages.push(ExternalProposal::new_add::<OpenMlsRustCrypto>(parsed, group.clone(), GroupEpoch::from(epoch), signer, SenderExtensionIndex::new(0)).unwrap());
        }
        messages.tls_serialize_detached().unwrap()
    }
}

use openmls::prelude::tls_codec::DeserializeBytes as _;

pub struct Client {
    pub engine: Engine,
    pub name: String,
}

impl Client {
    pub fn user(&self) -> UserId {
        self.engine.user_id()
    }
    pub fn device(&self) -> DeviceId {
        self.engine.device_id()
    }
}

/// The people and devices of a test, and the server's lists of them.
pub struct World {
    pub ds: Ds,
    pub clients: Vec<Client>,
    /// What `GET /v1/e2ee/users/{id}/devices` returns.
    pub directory: BTreeMap<UserId, Vec<DeviceRecord>>,
    clock: i64,
}

pub fn envelope(text: &str) -> Envelope {
    Envelope { text: text.to_owned(), sent_at: Some(1_730_000_000_000), ..Envelope::default() }
}

impl World {
    pub fn new() -> World {
        World { ds: Ds::new(), clients: Vec::new(), directory: BTreeMap::new(), clock: 0 }
    }

    /// A new person with a first device (trusted on first use).
    pub fn person(&mut self, name: &str) -> usize {
        let user = UserId::random().unwrap();
        self.add_client(name, user, None)
    }

    /// A new device for the person that `owner` is, cross-signed by `owner`'s device.
    pub fn second_device(&mut self, name: &str, owner: usize) -> usize {
        let user = self.clients[owner].user();
        self.add_client(name, user, Some(owner))
    }

    fn add_client(&mut self, name: &str, user: UserId, signer: Option<usize>) -> usize {
        let engine = Engine::with_identity(e2ee::identity::DeviceIdentity::generate(user).unwrap());
        let cross_signature = signer.map(|at| self.clients[at].engine.cross_sign(engine.device_id(), &engine.signature_key()).unwrap());
        self.clock += 1;
        self.directory.entry(user).or_default().push(DeviceRecord { device_id: engine.device_id(), signature_key: engine.signature_key(), cross_signature, created_at: self.clock });
        self.clients.push(Client { engine, name: name.to_owned() });
        self.clients.len() - 1
    }

    /// Every device fetches and verifies every person's device list.
    pub fn sync_trust(&mut self) {
        let lists: Vec<(UserId, Vec<DeviceRecord>)> = self.directory.iter().map(|(user, list)| (*user, list.clone())).collect();
        for client in &mut self.clients {
            for (user, list) in &lists {
                client.engine.verify_devices(*user, list).unwrap();
            }
        }
    }

    pub fn leaf(&mut self, asker: usize, target: usize) -> u32 {
        let (conv, device) = (self.conv(), self.clients[target].device());
        self.clients[asker].engine.leaf_index(conv, device).unwrap().expect("a member")
    }

    pub fn roster_len(&mut self, at: usize) -> usize {
        let conv = self.conv();
        self.clients[at].engine.roster(conv).unwrap().len()
    }

    pub fn conv(&self) -> ConversationId {
        self.ds.conv
    }

    pub fn epoch(&mut self, at: usize) -> u64 {
        let conv = self.conv();
        self.clients[at].engine.epoch(conv).unwrap()
    }

    /// The first device starts the group.
    pub fn start(&mut self, at: usize) {
        let (conv, key) = (self.conv(), self.ds.public());
        self.clients[at].engine.create_group(conv, &key).unwrap();
    }

    /// The server's proposals to add `adds` (each uploads a key package first) and remove `removes`
    /// (by client index), given to every `members` client; `committer` commits; the server accepts;
    /// the added join; the others follow. Returns what the committer produced.
    pub fn change(&mut self, members: &[usize], committer: usize, adds: &[usize], removes: &[usize]) -> CommitBundle {
        let conv = self.conv();
        let epoch = self.epoch(committer);
        let packages: Vec<Vec<u8>> = adds.iter().map(|at| self.clients[*at].engine.key_packages(1).unwrap().remove(0)).collect();
        let leaves: Vec<u32> = removes
            .iter()
            .map(|at| {
                let device = self.clients[*at].device();
                self.clients[committer].engine.leaf_index(conv, device).unwrap().expect("a member")
            })
            .collect();
        let refs: Vec<&[u8]> = packages.iter().map(Vec::as_slice).collect();
        let proposals = self.ds.proposals(&refs, &leaves, epoch);
        for at in members {
            let outcome = self.clients[*at].engine.handle_proposals(conv, &proposals).unwrap();
            assert!(outcome.rejected.is_empty(), "{} rejected {:?}", self.clients[*at].name, outcome.rejected);
        }
        let bundle = self.clients[committer].engine.commit_pending(conv).unwrap().expect("something to commit");
        // The server accepts the commit.
        self.clients[committer].engine.confirm_commit(conv).unwrap();
        let key = self.ds.public();
        for at in adds {
            let welcome = bundle.welcomes.iter().find(|w| w.device.device == self.clients[*at].device()).expect("a welcome");
            self.clients[*at].engine.handle_welcome(conv, &welcome.data, Some(&key)).unwrap();
        }
        for at in members {
            if *at == committer {
                continue;
            }
            let outcome = self.clients[*at].engine.handle_commit(conv, bundle.epoch, &bundle.commit).unwrap();
            assert!(!outcome.duplicate);
        }
        bundle
    }

    pub fn send(&mut self, from: usize, text: &str) -> Vec<u8> {
        let conv = self.conv();
        self.clients[from].engine.encrypt(conv, Kind::Message, None, &envelope(text)).unwrap().ciphertext
    }

    pub fn open(&mut self, at: usize, ciphertext: &[u8], from: usize) -> Result<Incoming, e2ee::Error> {
        let conv = self.conv();
        let (author, device) = (self.clients[from].user(), self.clients[from].device());
        self.clients[at].engine.decrypt(conv, ciphertext, &Claimed { author: Some(author), device: Some(device), kind: Kind::Message, target_id: None })
    }

    /// `from` says `text`; every client in `to` must read it.
    pub fn say(&mut self, from: usize, to: &[usize], text: &str) {
        let ciphertext = self.send(from, text);
        for at in to {
            let incoming = self.open(*at, &ciphertext, from).unwrap_or_else(|err| panic!("{} could not read: {err}", self.clients[*at].name));
            assert_eq!(incoming.envelope.text, text);
            assert!(incoming.sender_trusted);
        }
    }
}

/// A world of `devices` devices of different people (everyone is a member), as many as
/// `cap` at a time. Returns the world and the members.
pub fn group_of(devices: usize) -> (World, Vec<usize>) {
    let mut world = World::new();
    let all: Vec<usize> = (0..devices).map(|at| world.person(&format!("d{at}"))).collect();
    world.sync_trust();
    world.start(all[0]);
    let mut members = vec![all[0]];
    for next in &all[1..] {
        world.change(&members.clone(), all[0], &[*next], &[]);
        members.push(*next);
    }
    (world, all)
}

pub fn outcome_epoch(outcome: &CommitOutcome) -> u64 {
    outcome.epoch
}
