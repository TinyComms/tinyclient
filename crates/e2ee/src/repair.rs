//! Explicit, signed replacement of a conversation's cryptographic group. Bindings contain
//! public information only; they never establish identity trust or authorize historical Adds.

use crate::engine::{ConvMeta, Engine};
use crate::error::{Error, Result};
use crate::groups::parse_server_key;
use crate::ids::{ConversationId, DeviceId, UserId};
use crate::wire::sha256;
use crate::wire::Bytes;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use openmls::prelude as mls;
use openmls_traits::OpenMlsProvider as _;
use p256::ecdsa::{signature::Verifier as _, Signature, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};

const REPAIR_LABEL: &[u8] = b"tiny-e2ee-v1 group-repair\0";
const GENESIS_LABEL: &[u8] = b"tiny-e2ee-v1 group-genesis\0";
const MAX_NUMBER: u64 = i64::MAX as u64;

/// The fixed, signed public wire contract for one immediate generation replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u8,
    #[serde(deserialize_with = "canonical_conversation")]
    pub conversation_id: ConversationId,
    #[serde(deserialize_with = "bounded_number")]
    pub from_generation: u64,
    #[serde(deserialize_with = "bounded_number")]
    pub to_generation: u64,
    #[serde(deserialize_with = "canonical_conversation")]
    pub from_group_id: ConversationId,
    #[serde(deserialize_with = "canonical_conversation")]
    pub group_id: ConversationId,
    #[serde(deserialize_with = "bounded_number")]
    pub from_epoch: u64,
    #[serde(with = "hash_bytes")]
    pub previous_hash: [u8; 32],
    #[serde(with = "hash_bytes")]
    pub server_key_hash: [u8; 32],
    #[serde(deserialize_with = "canonical_user")]
    pub user_id: UserId,
    #[serde(deserialize_with = "canonical_device")]
    pub device_id: DeviceId,
    #[serde(serialize_with = "serialize_bytes", deserialize_with = "public_key")]
    pub signature_key: Vec<u8>,
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "signature_bytes"
    )]
    pub signature: Vec<u8>,
}

impl Binding {
    /// The 260-byte canonical payload. Signature bytes are deliberately excluded.
    pub fn canonical_payload(&self) -> Result<Vec<u8>> {
        if self.version != 1 {
            return Err(Error::UnsupportedVersion);
        }
        if self.from_generation >= MAX_NUMBER
            || self.to_generation != self.from_generation + 1
            || self.from_epoch > MAX_NUMBER
            || self.group_id == self.from_group_id
            || self.group_id == self.conversation_id
            || (self.from_generation == 0
                && (self.from_group_id != self.conversation_id
                    || self.previous_hash
                        != genesis_from_key_hash(self.conversation_id, &self.server_key_hash)))
        {
            return Err(Error::Malformed("repair binding"));
        }
        checked_key(&self.signature_key)?;
        let mut payload = Vec::with_capacity(260);
        payload.extend_from_slice(REPAIR_LABEL);
        payload.push(self.version);
        payload.extend_from_slice(&self.conversation_id.0);
        payload.extend_from_slice(&self.from_generation.to_be_bytes());
        payload.extend_from_slice(&self.to_generation.to_be_bytes());
        payload.extend_from_slice(&self.from_group_id.0);
        payload.extend_from_slice(&self.group_id.0);
        payload.extend_from_slice(&self.from_epoch.to_be_bytes());
        payload.extend_from_slice(&self.previous_hash);
        payload.extend_from_slice(&self.server_key_hash);
        payload.extend_from_slice(&self.user_id.0);
        payload.extend_from_slice(&self.device_id.0);
        payload.extend_from_slice(&self.signature_key);
        Ok(payload)
    }

    pub fn payload_hash(&self) -> Result<[u8; 32]> {
        Ok(sha256(&self.canonical_payload()?))
    }

    pub fn verify_signature(&self) -> Result<()> {
        let payload = self.canonical_payload()?;
        if !(8..=72).contains(&self.signature.len()) {
            return Err(Error::Malformed("repair signature"));
        }
        let signature = Signature::from_der(&self.signature)
            .map_err(|_| Error::Malformed("repair signature"))?;
        checked_key(&self.signature_key)?
            .verify(&payload, &signature)
            .map_err(|_| Error::BadSignature)
    }
}

/// Deterministic generation0 anchor, using the bare pinned external-sender public key.
pub fn genesis_hash(conversation: ConversationId, server_key: &[u8]) -> Result<[u8; 32]> {
    let (key, _) = parse_server_key(server_key)?;
    Ok(genesis_from_key_hash(conversation, &sha256(&key)))
}

fn genesis_from_key_hash(conversation: ConversationId, key_hash: &[u8; 32]) -> [u8; 32] {
    let mut payload = Vec::with_capacity(GENESIS_LABEL.len() + 48);
    payload.extend_from_slice(GENESIS_LABEL);
    payload.extend_from_slice(&conversation.0);
    payload.extend_from_slice(key_hash);
    sha256(&payload)
}

fn checked_key(key: &[u8]) -> Result<VerifyingKey> {
    if key.len() != 65 || key.first() != Some(&4) {
        return Err(Error::Malformed("repair signing key"));
    }
    VerifyingKey::from_sec1_bytes(key).map_err(|_| Error::Malformed("repair signing key"))
}

fn serialize_bytes<S: Serializer>(
    value: &[u8],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&STANDARD.encode(value))
}

fn decode_bytes<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<u8>, D::Error> {
    let text = String::deserialize(deserializer)?;
    let bytes = STANDARD.decode(&text).map_err(serde::de::Error::custom)?;
    if STANDARD.encode(&bytes) != text {
        return Err(serde::de::Error::custom("noncanonical base64"));
    }
    Ok(bytes)
}

fn public_key<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<u8>, D::Error> {
    let bytes = decode_bytes(deserializer)?;
    checked_key(&bytes).map_err(serde::de::Error::custom)?;
    Ok(bytes)
}

fn signature_bytes<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<u8>, D::Error> {
    let bytes = decode_bytes(deserializer)?;
    Signature::from_der(&bytes).map_err(serde::de::Error::custom)?;
    Ok(bytes)
}

fn bounded_number<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u64, D::Error> {
    let value = u64::deserialize(deserializer)?;
    if value > MAX_NUMBER {
        return Err(serde::de::Error::custom("repair number is too large"));
    }
    Ok(value)
}

macro_rules! canonical_id {
    ($fn:ident, $id:ident) => {
        fn $fn<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<$id, D::Error> {
            let text = String::deserialize(deserializer)?;
            let value = $id::parse(&text).map_err(serde::de::Error::custom)?;
            if value.to_string() != text {
                return Err(serde::de::Error::custom("noncanonical UUID"));
            }
            Ok(value)
        }
    };
}
canonical_id!(canonical_conversation, ConversationId);
canonical_id!(canonical_user, UserId);
canonical_id!(canonical_device, DeviceId);

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use crate::identity::{DeviceKey, DeviceRecord};
    use crate::store::{open_state, seal_state, StateKey};

    fn engine() -> (Engine, ConversationId, StateKey) {
        let mut engine = Engine::detached(UserId::random().unwrap()).unwrap();
        engine
            .verify_devices(
                engine.user_id(),
                &[DeviceRecord {
                    device_id: engine.device_id(),
                    signature_key: engine.signature_key(),
                    cross_signature: None,
                    created_at: 0,
                }],
            )
            .unwrap();
        let conversation = ConversationId::random().unwrap();
        engine
            .create_group(conversation, &DeviceKey::generate().unwrap().public_key())
            .unwrap();
        (engine, conversation, StateKey::generate().unwrap())
    }

    fn state(engine: &Engine, key: &StateKey) -> serde_json::Value {
        let blob = engine.snapshot(key, "schema").unwrap();
        serde_json::from_slice(&open_state(key, "schema", &blob).unwrap()).unwrap()
    }

    fn reopen(value: &serde_json::Value, key: &StateKey) -> Result<Engine> {
        let blob = seal_state(key, "schema", &serde_json::to_vec(value).unwrap()).unwrap();
        Engine::from_snapshot(key, "schema", &blob)
    }

    #[test]
    fn legacy_snapshots_migrate_but_security_metadata_is_never_downgrade_readable() {
        let (engine, conversation, key) = engine();
        let mut legacy = state(&engine, &key);
        assert_eq!(legacy["version"], 2);
        legacy["version"] = serde_json::json!(1);
        assert!(
            reopen(&legacy, &key).is_err(),
            "version1 cannot carry modern security fields"
        );
        legacy.as_object_mut().unwrap().remove("repair");
        let migrated = reopen(&legacy, &key).unwrap();
        assert_eq!(migrated.active_group(conversation).unwrap().generation, 0);
        assert!(migrated
            .active_group(conversation)
            .unwrap()
            .head_hash
            .is_some());
        assert_eq!(state(&migrated, &key)["version"], 2);
    }

    #[test]
    fn modern_snapshots_refuse_missing_or_inconsistent_generation_metadata() {
        let (engine, _, key) = engine();
        let mut missing = state(&engine, &key);
        missing.as_object_mut().unwrap().remove("repair");
        assert!(reopen(&missing, &key).is_err());
        let mut unmapped = state(&engine, &key);
        unmapped["repair"]["conversations"] = serde_json::json!({});
        assert!(reopen(&unmapped, &key).is_err());
        let mut missing_private = state(&engine, &key);
        missing_private["storage"] = serde_json::json!([]);
        assert!(reopen(&missing_private, &key).is_err());
    }

    #[test]
    fn retirement_and_exact_creator_private_state_are_required_on_reopen() {
        let (mut engine, conversation, key) = engine();
        let binding = engine.prepare_repair(conversation, 0, 7).unwrap();
        engine
            .mark_repair_submitted(conversation, binding.payload_hash().unwrap())
            .unwrap();
        let mut missing_candidate = state(&engine, &key);
        missing_candidate["meta"]
            .as_object_mut()
            .unwrap()
            .remove(&binding.group_id.to_string());
        assert!(reopen(&missing_candidate, &key).is_err());
        engine.accept_repair(&binding).unwrap();
        let accepted = state(&engine, &key);
        let restored = reopen(&accepted, &key).unwrap();
        assert_eq!(restored.active_group(conversation).unwrap().generation, 1);
        assert!(restored.active_group(binding.group_id).is_err());
        let mut no_retirement = accepted.clone();
        no_retirement["repair"]["retired"] = serde_json::json!([]);
        assert!(reopen(&no_retirement, &key).is_err());
        let mut lost_creator = accepted.clone();
        lost_creator["storage"] = serde_json::json!([]);
        assert!(reopen(&lost_creator, &key).is_err());
    }

    #[test]
    fn a_forgotten_legacy_conversation_is_not_a_first_contact_bootstrap() {
        let (mut engine, conversation, key) = engine();
        engine.forget_conversation(conversation).unwrap();
        let restored = reopen(&state(&engine, &key), &key).unwrap();
        assert!(restored
            .active_group(conversation)
            .unwrap()
            .head_hash
            .is_some());
    }
}

mod hash_bytes {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &[u8; 32],
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serialize_bytes(value, serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<[u8; 32], D::Error> {
        decode_bytes(deserializer)?
            .try_into()
            .map_err(|_| serde::de::Error::custom("hash length"))
    }
}

/// The public routing result. Existing MLS APIs take `group_id`, never the outer ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupGeneration {
    pub generation: u64,
    pub group_id: ConversationId,
    pub head_hash: Option<[u8; 32]>,
}

/// Public request state only. The candidate's private state remains in the sealed provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedRepair {
    pub binding: Binding,
    pub submitted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Generations {
    current: u64,
    base: u64,
    server_key: Bytes,
    groups: BTreeMap<u64, ConversationId>,
    bindings: BTreeMap<u64, Binding>,
}

impl Generations {
    fn legacy(conversation: ConversationId, key: Vec<u8>) -> Self {
        Self {
            current: 0,
            base: 0,
            server_key: key.into(),
            groups: BTreeMap::from([(0, conversation)]),
            bindings: BTreeMap::new(),
        }
    }

    fn head_hash(&self, conversation: ConversationId) -> Result<[u8; 32]> {
        if self.current == 0 {
            genesis_hash(conversation, &self.server_key)
        } else {
            self.bindings
                .get(&self.current)
                .ok_or(Error::Malformed("missing generation binding"))?
                .payload_hash()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RepairState {
    conversations: BTreeMap<ConversationId, Generations>,
    prepared: BTreeMap<ConversationId, PreparedRepair>,
    retired: BTreeSet<ConversationId>,
    pub(crate) failed_writes: BTreeSet<ConversationId>,
    // A validated service result is knowledge independent of a local candidate. Persist it
    // before changing active state; candidate retries/discards must never erase it.
    observed_successors: BTreeMap<ConversationId, Binding>,
}

impl RepairState {
    pub(crate) fn legacy(meta: &BTreeMap<ConversationId, ConvMeta>) -> Result<Self> {
        let mut state = Self::default();
        for (conversation, meta) in meta {
            let (key, _) = parse_server_key(&meta.server_key)?;
            state
                .conversations
                .insert(*conversation, Generations::legacy(*conversation, key));
        }
        Ok(state)
    }
}

impl Engine {
    pub fn active_group(&self, conversation: ConversationId) -> Result<GroupGeneration> {
        match self.repair.conversations.get(&conversation) {
            Some(history) => Ok(GroupGeneration {
                generation: history.current,
                group_id: *history
                    .groups
                    .get(&history.current)
                    .ok_or(Error::Malformed("missing active generation"))?,
                head_hash: Some(history.head_hash(conversation)?),
            }),
            None if self.repair.observed_successors.contains_key(&conversation) => {
                Err(Error::RepairPending)
            }
            None if self.repair_id_used(conversation) => Err(Error::InvalidRepair(
                "internal group is not an outer conversation",
            )),
            None => Ok(GroupGeneration {
                generation: 0,
                group_id: conversation,
                head_hash: None,
            }),
        }
    }

    pub fn group_at(
        &self,
        conversation: ConversationId,
        generation: u64,
    ) -> Result<ConversationId> {
        if let Some(history) = self.repair.conversations.get(&conversation) {
            return history
                .groups
                .get(&generation)
                .copied()
                .ok_or(Error::NoGroup);
        }
        if generation == 0 && !self.repair_id_used(conversation) {
            Ok(conversation)
        } else {
            Err(Error::NoGroup)
        }
    }

    pub fn prepared_repair(&self, conversation: ConversationId) -> Option<PreparedRepair> {
        self.repair.prepared.get(&conversation).cloned()
    }

    /// Caller must freshly verify ordinary directory state and obtain owner approval first.
    pub fn prepare_repair(
        &mut self,
        conversation: ConversationId,
        expected_generation: u64,
        observed_epoch: u64,
    ) -> Result<Binding> {
        if let Some(observed) = self.repair.observed_successors.get(&conversation) {
            if !self
                .repair
                .prepared
                .get(&conversation)
                .is_some_and(|candidate| {
                    candidate.binding.payload_hash().ok() == observed.payload_hash().ok()
                })
            {
                return Err(Error::RepairPending);
            }
        }
        if let Some(prepared) = self.repair.prepared.get(&conversation) {
            if prepared.binding.from_generation != expected_generation {
                return Err(Error::InvalidRepair("generation changed"));
            }
            let binding = prepared.binding.clone();
            self.check_repair_signer(&binding)?;
            // Even a retry must not return a request after an unresolved failed write.
            self.repair_transaction(conversation, |_| Ok(()))?;
            return Ok(binding);
        }
        let active = self.active_group(conversation)?;
        if active.generation != expected_generation
            || expected_generation >= MAX_NUMBER
            || observed_epoch > MAX_NUMBER
        {
            return Err(Error::InvalidRepair("generation changed or out of range"));
        }
        let meta = self.meta.get(&active.group_id).ok_or(Error::NoGroup)?;
        if meta.removed {
            return Err(Error::NotAMember);
        }
        if meta.self_removal_pending {
            return Err(Error::SelfRemoval);
        }
        if meta.server_key_alert {
            return Err(Error::ServerKeyChanged);
        }
        let key = meta.server_key.0.clone();
        let high = meta.high_epoch.max(self.epoch(active.group_id)?);
        if observed_epoch < high {
            return Err(Error::InvalidRepair("old epoch went backwards"));
        }
        let own = self.device_ref();
        if !self
            .trust
            .is_trusted(own.user, own.device, &own.signature_key)
            || !self.roster(active.group_id)?.contains(&own)
        {
            return Err(Error::InvalidRepair(
                "initiator is not an approved old member",
            ));
        }
        let group_id = loop {
            let candidate = ConversationId::random()?;
            if !self.repair_id_used(candidate) {
                break candidate;
            }
        };
        let mut binding = Binding {
            version: 1,
            conversation_id: conversation,
            from_generation: active.generation,
            to_generation: active.generation + 1,
            from_group_id: active.group_id,
            group_id,
            from_epoch: observed_epoch,
            previous_hash: active.head_hash.ok_or(Error::NoGroup)?,
            server_key_hash: sha256(&key),
            user_id: own.user,
            device_id: own.device,
            signature_key: own.signature_key,
            signature: Vec::new(),
        };
        binding.signature = self.identity.key.sign(&binding.canonical_payload()?);
        self.repair_transaction(conversation, |engine| {
            engine.create_group_state(group_id, &key)?;
            engine.repair.prepared.insert(
                conversation,
                PreparedRepair {
                    binding: binding.clone(),
                    submitted: false,
                },
            );
            Ok(binding)
        })
    }

    /// Must succeed before the caller starts any POST of this binding.
    pub fn mark_repair_submitted(
        &mut self,
        conversation: ConversationId,
        hash: [u8; 32],
    ) -> Result<()> {
        let prepared = self
            .repair
            .prepared
            .get(&conversation)
            .ok_or(Error::InvalidRepair("no prepared repair"))?;
        if prepared.binding.payload_hash()? != hash {
            return Err(Error::InvalidRepair("prepared binding differs"));
        }
        if self
            .repair
            .observed_successors
            .get(&conversation)
            .is_some_and(|observed| observed.payload_hash().ok() != Some(hash))
        {
            return Err(Error::RepairPending);
        }
        self.check_repair_signer(&prepared.binding)?;
        self.repair_transaction(conversation, |engine| {
            engine
                .repair
                .prepared
                .get_mut(&conversation)
                .ok_or(Error::InvalidRepair("no prepared repair"))?
                .submitted = true;
            Ok(())
        })
    }

    pub fn discard_prepared_repair(
        &mut self,
        conversation: ConversationId,
        hash: [u8; 32],
    ) -> Result<()> {
        let prepared = self
            .repair
            .prepared
            .get(&conversation)
            .ok_or(Error::InvalidRepair("no prepared repair"))?;
        if prepared.binding.payload_hash()? != hash || prepared.submitted {
            return Err(Error::InvalidRepair("submitted repair remains uncertain"));
        }
        self.repair_transaction(conversation, |engine| engine.remove_prepared(conversation))
    }

    /// Accept only this device's persisted candidate or an independently authorized successor.
    /// An HTTP response is not authenticated by this method; callers supply the service's result.
    pub fn accept_repair(&mut self, binding: &Binding) -> Result<GroupGeneration> {
        binding.verify_signature()?;
        let observed_match = self.observed_successor_matches(binding)?;
        let conversation = binding.conversation_id;
        let active = self.active_group(conversation)?;
        if active.generation == binding.to_generation
            && active.head_hash == Some(binding.payload_hash()?)
        {
            return Ok(active);
        }
        if active.generation != binding.from_generation
            || active.group_id != binding.from_group_id
            || active.head_hash != Some(binding.previous_hash)
        {
            return Err(Error::InvalidRepair(
                "binding is not the immediate successor",
            ));
        }
        let meta = self.meta.get(&active.group_id).ok_or(Error::NoGroup)?;
        if meta.removed {
            return Err(Error::NotAMember);
        }
        if meta.server_key_alert {
            return Err(Error::ServerKeyChanged);
        }
        if sha256(&meta.server_key) != binding.server_key_hash {
            return Err(Error::ServerKeyChanged);
        }
        let high = meta.high_epoch.max(self.epoch(active.group_id)?);
        if binding.from_epoch < high {
            return Err(Error::InvalidRepair("old epoch went backwards"));
        }
        self.check_repair_signer(binding)?;
        let signer = crate::DeviceRef {
            user: binding.user_id,
            device: binding.device_id,
            signature_key: binding.signature_key.clone(),
        };
        if !self.roster(active.group_id)?.contains(&signer) {
            return Err(Error::InvalidRepair(
                "signer was not in the authenticated old roster",
            ));
        }
        let prepared = self.prepared_repair(conversation);
        let own_candidate = prepared
            .as_ref()
            .is_some_and(|p| p.binding.payload_hash().ok() == binding.payload_hash().ok());
        if own_candidate && !prepared.as_ref().is_some_and(|p| p.submitted) {
            return Err(Error::InvalidRepair(
                "creator repair has not been submitted",
            ));
        }
        if !own_candidate
            && self.repair_namespace_used(binding.group_id, observed_match.then_some(binding))
        {
            return Err(Error::InvalidRepair("group identifier was already used"));
        }
        if signer == self.device_ref() && !own_candidate {
            return Err(Error::InvalidRepair("creator private state is missing"));
        }
        self.record_observed_successor(binding)?;
        self.repair_transaction(conversation, |engine| {
            if prepared.is_some() && !own_candidate {
                engine.remove_prepared(conversation)?;
            }
            if own_candidate {
                engine.load_group(binding.group_id)?;
                if engine.epoch(binding.group_id)? != 0
                    || engine.roster(binding.group_id)? != vec![engine.device_ref()]
                {
                    return Err(Error::InvalidRepair(
                        "creator state changed before acceptance",
                    ));
                }
                engine.repair.prepared.remove(&conversation);
            } else {
                let key = engine
                    .meta
                    .get(&active.group_id)
                    .ok_or(Error::NoGroup)?
                    .server_key
                    .clone();
                engine.meta.insert(
                    binding.group_id,
                    ConvMeta {
                        server_key: key,
                        removed: true,
                        ..ConvMeta::default()
                    },
                );
            }
            engine.repair.retired.insert(active.group_id);
            let history = engine
                .repair
                .conversations
                .get_mut(&conversation)
                .ok_or(Error::NoGroup)?;
            history.current = binding.to_generation;
            history
                .groups
                .insert(binding.to_generation, binding.group_id);
            history
                .bindings
                .insert(binding.to_generation, binding.clone());
            engine.repair.observed_successors.remove(&conversation);
            engine.active_group(conversation)
        })
    }

    /// First group bootstrap only; all signer trust must already exist independently.
    pub fn bootstrap_repair(
        &mut self,
        binding: &Binding,
        independently_observed_server_key: &[u8],
    ) -> Result<GroupGeneration> {
        binding.verify_signature()?;
        let observed_match = self.observed_successor_matches(binding)?;
        let conversation = binding.conversation_id;
        let exception = observed_match.then_some(binding);
        if self.repair_namespace_used(conversation, exception)
            || self.repair_namespace_used(binding.group_id, exception)
        {
            return Err(Error::InvalidRepair(
                "conversation already has authenticated state",
            ));
        }
        let (key, _) = parse_server_key(independently_observed_server_key)?;
        if sha256(&key) != binding.server_key_hash {
            return Err(Error::ServerKeyChanged);
        }
        self.check_repair_signer(binding)?;
        if binding.device_id == self.device_id() && binding.user_id == self.user_id() {
            return Err(Error::InvalidRepair("creator private state is missing"));
        }
        self.record_observed_successor(binding)?;
        self.repair_transaction(conversation, |engine| {
            engine.meta.insert(
                binding.group_id,
                ConvMeta {
                    server_key: key.clone().into(),
                    removed: true,
                    ..ConvMeta::default()
                },
            );
            engine.repair.conversations.insert(
                conversation,
                Generations {
                    current: binding.to_generation,
                    base: binding.to_generation,
                    server_key: key.into(),
                    groups: BTreeMap::from([(binding.to_generation, binding.group_id)]),
                    bindings: BTreeMap::from([(binding.to_generation, binding.clone())]),
                },
            );
            engine.repair.observed_successors.remove(&conversation);
            engine.active_group(conversation)
        })
    }

    fn check_repair_signer(&self, binding: &Binding) -> Result<()> {
        if !self
            .trust
            .is_trusted(binding.user_id, binding.device_id, &binding.signature_key)
        {
            return Err(Error::InvalidRepair("signer is not currently approved"));
        }
        Ok(())
    }

    fn observed_successor_matches(&self, binding: &Binding) -> Result<bool> {
        match self
            .repair
            .observed_successors
            .get(&binding.conversation_id)
        {
            Some(observed) if observed.payload_hash()? != binding.payload_hash()? => Err(
                Error::InvalidRepair("a different accepted successor is already recorded"),
            ),
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }

    fn record_observed_successor(&mut self, binding: &Binding) -> Result<()> {
        self.observed_successor_matches(binding)?;
        let previous = self
            .repair
            .observed_successors
            .insert(binding.conversation_id, binding.clone());
        // A public operation must never save a state its own reader would reject. An invalid
        // new observation is rolled back; previously validated acceptance knowledge remains.
        if let Err(error) = self.validate_repair_state() {
            if let Some(previous) = previous {
                self.repair
                    .observed_successors
                    .insert(binding.conversation_id, previous);
            } else {
                self.repair
                    .observed_successors
                    .remove(&binding.conversation_id);
            }
            return Err(error);
        }
        // Do not roll this knowledge back if saving fails. The old state remains locked in
        // memory; any subsequent successful save also durably retains the exact successor.
        if let Err(error) = self.persist(true) {
            self.repair.failed_writes.insert(binding.conversation_id);
            return Err(error);
        }
        Ok(())
    }

    fn remove_prepared(&mut self, conversation: ConversationId) -> Result<()> {
        let candidate = self
            .repair
            .prepared
            .remove(&conversation)
            .ok_or(Error::InvalidRepair("no prepared repair"))?
            .binding
            .group_id;
        self.load_group(candidate)?;
        self.groups
            .get_mut(&candidate)
            .ok_or(Error::NoGroup)?
            .delete(self.provider.storage())
            .map_err(Error::mls)?;
        self.groups.remove(&candidate);
        self.meta.remove(&candidate);
        Ok(())
    }

    fn repair_id_used(&self, group: ConversationId) -> bool {
        self.repair_namespace_used(group, None)
    }

    // The only exception is the exact observation being retried. This does not exempt an
    // accepted head, metadata, another observation, or any prepared/retired private group.
    fn repair_namespace_used(&self, group: ConversationId, exception: Option<&Binding>) -> bool {
        self.meta.contains_key(&group)
            || self.repair.retired.contains(&group)
            || self.repair.conversations.contains_key(&group)
            || self
                .repair
                .conversations
                .values()
                .any(|h| h.groups.values().any(|id| *id == group))
            || self
                .repair
                .prepared
                .values()
                .any(|p| p.binding.group_id == group)
            || self
                .repair
                .observed_successors
                .iter()
                .any(|(outer, binding)| {
                    (*outer == group || binding.group_id == group)
                        && !exception.is_some_and(|expected| {
                            expected.conversation_id == *outer
                                && expected.payload_hash().ok() == binding.payload_hash().ok()
                        })
                })
    }

    pub(crate) fn register_legacy_group(&mut self, group: ConversationId) -> Result<()> {
        if self
            .repair
            .conversations
            .values()
            .any(|h| h.groups.values().any(|id| *id == group))
        {
            let key = self
                .meta
                .get(&group)
                .ok_or(Error::NoGroup)?
                .server_key
                .0
                .clone();
            self.repair_server_pin_allowed(group, &key)?;
            self.update_legacy_server_pin(group, key);
            return Ok(());
        }
        if self
            .repair
            .conversations
            .get(&group)
            .is_some_and(|history| history.current > 0)
        {
            return Err(Error::InvalidRepair(
                "outer conversation already has a positive generation",
            ));
        }
        let key = self
            .meta
            .get(&group)
            .ok_or(Error::NoGroup)?
            .server_key
            .0
            .clone();
        self.repair
            .conversations
            .insert(group, Generations::legacy(group, key));
        Ok(())
    }

    pub(crate) fn repair_server_pin_allowed(
        &self,
        group: ConversationId,
        key: &[u8],
    ) -> Result<()> {
        self.repair_outbound_allowed(group)?;
        if self.repair.conversations.values().any(|history| {
            history.current > 0
                && history.groups.values().any(|id| *id == group)
                && &history.server_key[..] != key
        }) {
            return Err(Error::ServerKeyChanged);
        }
        Ok(())
    }

    pub(crate) fn update_legacy_server_pin(&mut self, group: ConversationId, key: Vec<u8>) {
        if let Some(history) = self.repair.conversations.get_mut(&group) {
            if history.current == 0 {
                history.server_key = key.into();
            }
        }
    }

    pub(crate) fn repair_outbound_allowed(&self, group: ConversationId) -> Result<()> {
        if self.repair.retired.contains(&group) {
            return Err(Error::RetiredGroup);
        }
        if self.repair.failed_writes.iter().any(|outer| {
            *outer == group
                || self
                    .repair
                    .conversations
                    .get(outer)
                    .is_some_and(|history| history.groups.values().any(|id| *id == group))
        }) || self
            .repair
            .observed_successors
            .iter()
            .any(|(outer, binding)| {
                *outer == group || binding.group_id == group || binding.from_group_id == group
            })
            || self
                .repair
                .prepared
                .values()
                .any(|p| p.binding.group_id == group || p.binding.from_group_id == group)
        {
            return Err(Error::RepairPending);
        }
        Ok(())
    }

    pub(crate) fn repair_receive_allowed(&self, group: ConversationId) -> Result<()> {
        if self
            .repair
            .prepared
            .values()
            .any(|p| p.binding.group_id == group)
        {
            return Err(Error::RepairPending);
        }
        Ok(())
    }

    pub(crate) fn repair_replacement_allowed(&self, group: ConversationId) -> Result<()> {
        self.repair_outbound_allowed(group)?;
        if self.repair.conversations.iter().any(|(conversation, h)| {
            h.current > 0 && (*conversation == group || h.groups.values().any(|id| *id == group))
        }) {
            return Err(Error::InvalidRepair("generation group cannot be recreated"));
        }
        Ok(())
    }

    pub(crate) fn repair_welcome_allowed(&self, group: ConversationId) -> Result<()> {
        self.repair_outbound_allowed(group)?;
        if self
            .repair
            .conversations
            .get(&group)
            .is_some_and(|history| history.current > 0)
        {
            return Err(Error::InvalidRepair(
                "Welcome names the old outer group instead of its accepted generation",
            ));
        }
        Ok(())
    }

    pub(crate) fn repair_forget_allowed(&self, group: ConversationId) -> Result<()> {
        self.repair_replacement_allowed(group)
    }

    pub(crate) fn validate_repair_state(&mut self) -> Result<()> {
        let mut known = BTreeSet::new();
        let mut retired = BTreeSet::new();
        for (conversation, history) in &self.repair.conversations {
            let (key, _) = parse_server_key(&history.server_key)?;
            if history.base > history.current
                || history.current > MAX_NUMBER
                || history.groups.len() as u64 != history.current - history.base + 1
            {
                return Err(Error::Malformed("generation history"));
            }
            let mut previous = None;
            for generation in history.base..=history.current {
                let group = *history
                    .groups
                    .get(&generation)
                    .ok_or(Error::Malformed("missing generation group"))?;
                if group != *conversation && self.repair.conversations.contains_key(&group) {
                    return Err(Error::Malformed(
                        "generation group aliases another outer conversation",
                    ));
                }
                if !known.insert(group) {
                    return Err(Error::Malformed("reused generation group"));
                }
                if generation < history.current {
                    retired.insert(group);
                }
                if generation == 0 {
                    if group != *conversation {
                        return Err(Error::Malformed("legacy generation group"));
                    }
                    previous = Some(genesis_hash(*conversation, &key)?);
                } else {
                    let binding = history
                        .bindings
                        .get(&generation)
                        .ok_or(Error::Malformed("missing generation binding"))?;
                    binding.verify_signature()?;
                    if binding.conversation_id != *conversation
                        || binding.to_generation != generation
                        || binding.group_id != group
                        || binding.server_key_hash != sha256(&key)
                        || previous.is_some_and(|hash| hash != binding.previous_hash)
                        || (generation > history.base
                            && history.groups.get(&(generation - 1))
                                != Some(&binding.from_group_id))
                    {
                        return Err(Error::Malformed("generation binding chain"));
                    }
                    previous = Some(binding.payload_hash()?);
                }
                if let Some(meta) = self.meta.get(&group) {
                    if meta.server_key.0 != key {
                        return Err(Error::Malformed("generation server pin"));
                    }
                    if !meta.removed
                        && mls::MlsGroup::load(
                            self.provider.storage(),
                            &mls::GroupId::from_slice(&group.0),
                        )
                        .map_err(Error::mls)?
                        .is_none()
                    {
                        return Err(Error::Malformed("missing generation private state"));
                    }
                } else if generation > 0 {
                    return Err(Error::Malformed("missing generation metadata"));
                }
            }
            if history.bindings.len() as u64
                != history.current - history.base + u64::from(history.base > 0)
            {
                return Err(Error::Malformed("extra generation bindings"));
            }
        }
        if retired != self.repair.retired {
            return Err(Error::Malformed("generation retirement"));
        }
        let prepared_states = self.repair.prepared.clone();
        for (conversation, prepared) in &prepared_states {
            let binding = &prepared.binding;
            binding.verify_signature()?;
            let active = self.active_group(*conversation)?;
            if binding.conversation_id != *conversation
                || binding.from_generation != active.generation
                || binding.from_group_id != active.group_id
                || Some(binding.previous_hash) != active.head_hash
                || binding.user_id != self.user_id()
                || binding.device_id != self.device_id()
                || binding.signature_key != self.signature_key()
                || self.repair.conversations.contains_key(&binding.group_id)
                || !known.insert(binding.group_id)
            {
                return Err(Error::Malformed("prepared generation"));
            }
            let meta = self
                .meta
                .get(&binding.group_id)
                .ok_or(Error::Malformed("missing creator metadata"))?;
            if sha256(&meta.server_key) != binding.server_key_hash || meta.removed {
                return Err(Error::Malformed("creator pin or membership"));
            }
            self.load_group(binding.group_id)?;
            if self.epoch(binding.group_id)? != 0
                || self.roster(binding.group_id)? != vec![self.device_ref()]
            {
                return Err(Error::Malformed("creator private state"));
            }
        }
        for (conversation, binding) in &self.repair.observed_successors {
            binding.verify_signature()?;
            if binding.conversation_id != *conversation {
                return Err(Error::Malformed("observed successor conversation"));
            }
            if let Some(history) = self.repair.conversations.get(conversation) {
                if binding.from_generation != history.current
                    || history.groups.get(&history.current) != Some(&binding.from_group_id)
                    || binding.previous_hash != history.head_hash(*conversation)?
                    || binding.server_key_hash != sha256(&history.server_key)
                {
                    return Err(Error::Malformed("observed successor anchor"));
                }
            } else if self.repair_namespace_used(*conversation, Some(binding)) {
                return Err(Error::Malformed("observed bootstrap namespace"));
            }
            if (known.contains(&binding.group_id)
                && !self
                    .repair
                    .prepared
                    .get(conversation)
                    .is_some_and(|prepared| {
                        prepared.binding.payload_hash().ok() == binding.payload_hash().ok()
                    }))
                || self.repair.conversations.contains_key(&binding.group_id)
                || self
                    .repair
                    .observed_successors
                    .iter()
                    .any(|(other, observed)| {
                        *other != *conversation
                            && (*other == binding.group_id || observed.group_id == binding.group_id)
                    })
            {
                return Err(Error::Malformed("observed successor group reused"));
            }
        }
        if self.meta.keys().any(|group| !known.contains(group)) {
            return Err(Error::Malformed("unmapped group metadata"));
        }
        Ok(())
    }
}
