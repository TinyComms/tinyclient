//! What a person's devices tell each other, over the person's own end-to-end encrypted
//! conversation (`POST /v1/e2ee/self`, `e2ee_self`): a conversation whose only member is the
//! person, so its MLS group holds exactly their active devices. The server cannot read it and
//! deletes its messages after `e2ee_self_ttl_secs` (7 days by default).
//!
//! - **Gap fill.** A device that cannot open a message (a past epoch it no longer holds, a group it
//!   joined later) asks its siblings for it (`want`). One that opened it answers with what its
//!   cache holds (`have`), which the asking device keeps, marked as from that device.
//! - **Verified marks.** Marking a safety number as compared, or taking that back, is told to the
//!   siblings (`verified`), so each device shows the same.
//! - **Read state** needs nothing here: read positions are the server's, per person, and every
//!   session hears `read.updated`.
//!
//! A message here is an ordinary encrypted message whose text is `tiny-sync-1 ` and JSON. Only
//! messages from the person's own other devices count; anything else is dropped.

use super::{lock, Device, Opened, Read, Row, Shared};
use e2ee::{Envelope, Kind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

/// What starts the text of a sync message.
const PREFIX: &str = "tiny-sync-1 ";
/// Ids asked at once.
const ASK_AT_ONCE: usize = 50;
/// A fragment is base64 inside the sync JSON inside an envelope. Four KiB leaves ample room
/// for both layers of encoding; ordinary batches are checked with the real envelope codec.
const FRAGMENT_BYTES: usize = 4 * 1024;
const MAX_FRAGMENTS: usize = 16;
const MAX_ROW_BYTES: usize = FRAGMENT_BYTES * MAX_FRAGMENTS;
const MAX_ASSEMBLIES: usize = 32;
/// A message asked for is not asked for again sooner.
const ASK_AGAIN: Duration = Duration::from_secs(10 * 60);
/// Tries at asking, and the wait between them: past the server's commit batching.
const RETRIES: usize = 4;
const RETRY_AFTER: Duration = Duration::from_secs(35);
/// Self messages read at once when catching up.
const CATCH_UP: usize = 100;

/// No `Debug`: it carries plaintext.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub(super) enum Sync {
    Want {
        conv: String,
        ids: Vec<String>,
    },
    Have {
        conv: String,
        rows: Vec<(String, Opened)>,
    },
    /// A large opened row, split without losing attachment keys or requiring a larger envelope.
    HavePart {
        conv: String,
        id: String,
        hash: String,
        part: u32,
        total: u32,
        data: String,
    },
    /// `number` is the safety number marked as compared; `None` takes the mark back.
    Verified {
        user: String,
        number: Option<String>,
    },
}

impl Sync {
    pub(super) fn text(&self) -> Option<String> {
        serde_json::to_string(self)
            .ok()
            .map(|json| format!("{PREFIX}{json}"))
    }

    pub(super) fn parse(text: &str) -> Option<Sync> {
        serde_json::from_str(text.strip_prefix(PREFIX)?).ok()
    }

    fn fits(&self) -> bool {
        self.text().is_some_and(|text| Envelope::message(text).encode(Kind::Message).is_ok())
    }
}

/// `rows` as bounded wire envelopes. Large rows use hashed fragments, including rows whose
/// text grows when the sync JSON is escaped a second time inside the encrypted envelope.
pub(super) fn answers(conv: &str, rows: Vec<(String, Opened)>) -> Vec<Sync> {
    let mut out = Vec::new();
    let mut batch: Vec<(String, Opened)> = Vec::new();
    for (id, opened) in rows {
        let Ok(bytes) = serde_json::to_vec(&opened) else {
            continue;
        };
        if bytes.len() > MAX_ROW_BYTES {
            continue;
        }
        let mut candidate = batch.clone();
        candidate.push((id.clone(), opened.clone()));
        if (Sync::Have { conv: conv.to_owned(), rows: candidate.clone() }).fits() {
            batch = candidate;
            continue;
        }
        if !batch.is_empty() {
            out.push(Sync::Have {
                conv: conv.to_owned(),
                rows: std::mem::take(&mut batch),
            });
        }
        let single = Sync::Have { conv: conv.to_owned(), rows: vec![(id.clone(), opened)] };
        if single.fits() {
            if let Sync::Have { rows, .. } = single {
                batch = rows;
            }
            continue;
        }
        let hash = super::b64(&Sha256::digest(&bytes));
        let total = bytes.len().div_ceil(FRAGMENT_BYTES) as u32;
        for (part, bytes) in bytes.chunks(FRAGMENT_BYTES).enumerate() {
            let reply = Sync::HavePart {
                conv: conv.to_owned(), id: id.clone(), hash: hash.clone(),
                part: part as u32, total, data: super::b64(bytes),
            };
            if reply.fits() {
                out.push(reply);
            }
        }
    }
    if !batch.is_empty() {
        out.push(Sync::Have {
            conv: conv.to_owned(),
            rows: batch,
        });
    }
    out
}

/// What this device keeps about its siblings while it runs.
#[derive(Default)]
pub(super) struct Siblings {
    /// Asked for, and when: not asked again for a while.
    asked: HashMap<(String, String), Instant>,
    /// Waiting to be asked: by conversation.
    wanted: Vec<(String, Vec<String>)>,
    /// Self messages already acted on.
    seen: BTreeSet<String>,
    /// At most two MiB of partial row data, separated by sending device and digest. Unfinished
    /// rows expire so retries cannot accumulate plaintext indefinitely.
    parts: HashMap<(String, String, String, String), Parts>,
}

struct Parts {
    at: Instant,
    chunks: Vec<Option<Vec<u8>>>,
}

impl Device {
    /// The person's own conversation, made the first time. `None` where the server has none.
    pub(super) fn self_conversation(&mut self) -> Option<String> {
        if !self.self_capable || !self.ready() {
            return None;
        }
        if let Some(id) = self.notes.self_conv.clone() {
            return Some(id);
        }
        let id = match self.ask("POST", "/v1/e2ee/self", None) {
            Ok((200 | 201, answer)) => answer
                .get("conversation_id")
                .and_then(Value::as_str)
                .map(str::to_owned)?,
            Ok((status, _)) => {
                tracing::warn!("chatd: Tiny e2ee: the server did not make this person's own conversation ({status})");
                return None;
            }
            Err(why) => {
                tracing::warn!("chatd: Tiny e2ee: {why}");
                return None;
            }
        };
        if !super::super::safe_id(&id) {
            return None;
        }
        self.encrypted.insert(id.clone(), true);
        self.notes.self_conv = Some(id.clone());
        self.save_notes();
        Some(id)
    }

    pub(crate) fn is_self(&self, conversation_id: &str) -> bool {
        self.notes.self_conv.as_deref() == Some(conversation_id)
    }

    /// Tells the siblings. False when it could not go now (this device is not in its own group
    /// yet, or the server could not be reached).
    fn tell(&mut self, sync: &Sync) -> bool {
        let Some(conv) = self.self_conversation() else {
            return false;
        };
        let Some(text) = sync.text() else {
            return true;
        };
        let envelope = Envelope {
            text,
            reply_to: None,
            attachments: Vec::new(),
            sent_at: None,
        };
        let Ok(client_id) = super::attachment_id() else {
            return false;
        };
        match self.post(&conv, &client_id, Kind::Message, None, &envelope, &[]) {
            Ok(_) => true,
            Err(why) => {
                tracing::warn!("chatd: Tiny e2ee: siblings: {why}");
                false
            }
        }
    }

    /// Messages of `conversation_id` that did not open here: asked of the siblings by
    /// [`Device::ask_siblings`], unless they were asked lately.
    pub(super) fn want(&mut self, conversation_id: &str, ids: Vec<String>) {
        if !self.self_capable || self.is_self(conversation_id) {
            return;
        }
        let now = Instant::now();
        let fresh: Vec<String> = ids
            .into_iter()
            .filter(|id| {
                let key = (conversation_id.to_owned(), id.clone());
                match self.siblings.asked.get(&key) {
                    Some(at) if now.duration_since(*at) < ASK_AGAIN => false,
                    _ => {
                        self.siblings.asked.insert(key, now);
                        true
                    }
                }
            })
            .collect();
        if !fresh.is_empty() {
            self.siblings
                .wanted
                .push((conversation_id.to_owned(), fresh));
        }
    }

    pub(crate) fn has_wants(&self) -> bool {
        !self.siblings.wanted.is_empty()
    }

    /// Asks the siblings for what is waiting. What could not go stays waiting.
    pub(crate) fn ask_siblings(&mut self) {
        let mut left = Vec::new();
        for (conv, ids) in std::mem::take(&mut self.siblings.wanted) {
            for chunk in ids.chunks(ASK_AT_ONCE) {
                if !self.tell(&Sync::Want {
                    conv: conv.clone(),
                    ids: chunk.to_vec(),
                }) {
                    left.push((conv.clone(), chunk.to_vec()));
                }
            }
        }
        self.siblings.wanted = left;
    }

    /// A safety number marked or unmarked here: the siblings mark it too.
    pub(super) fn tell_verified(&mut self, user: &str, number: Option<String>) {
        self.tell(&Sync::Verified {
            user: user.to_owned(),
            number,
        });
    }

    /// One message of the person's own conversation. Answers the conversations that gained
    /// messages, so the caller shows them again.
    pub(crate) fn sibling_said(&mut self, row: &Row) -> Vec<String> {
        let Some(conv) = self.notes.self_conv.clone() else {
            return Vec::new();
        };
        let mine = self
            .engine
            .as_ref()
            .map(|engine| engine.device_id().to_string());
        // Only the person's own other devices. MLS has checked the sender is who the row says.
        if row.author_id != self.saved.user_id
            || mine.as_deref() == Some(row.device_id)
            || self.siblings.seen.contains(row.id)
        {
            return Vec::new();
        }
        let opened = match self.read(&conv, std::slice::from_ref(row)).remove(row.id) {
            Some(Read::Opened(opened)) => opened,
            _ => return Vec::new(),
        };
        let Some(sync) = Sync::parse(&opened.text) else {
            return Vec::new();
        };
        self.siblings.seen.insert(row.id.to_owned());
        self.act(sync, row.device_id)
    }

    fn act(&mut self, sync: Sync, from_device: &str) -> Vec<String> {
        match sync {
            Sync::Want { conv, ids } => {
                if !super::super::safe_id(&conv) || self.is_self(&conv) {
                    return Vec::new();
                }
                for answer in self.answers_for(&conv, &ids) {
                    let _ = self.tell(&answer);
                }
                Vec::new()
            }
            Sync::Have { conv, rows } => {
                if !super::super::safe_id(&conv) || self.is_self(&conv) {
                    return Vec::new();
                }
                let mut added = false;
                for (id, mut opened) in rows {
                    let known = self
                        .cache(&conv)
                        .is_none_or(|cache| cache.rows.contains_key(&id));
                    if known || !super::super::safe_id(&id) {
                        continue;
                    }
                    opened.imported_from = Some(from_device.chars().take(64).collect());
                    self.remember(&conv, &id, opened);
                    added = true;
                }
                if added {
                    vec![conv]
                } else {
                    Vec::new()
                }
            }
            Sync::HavePart { conv, id, hash, part, total, data } => {
                self.take_part(conv, id, hash, part, total, data, from_device)
            }
            Sync::Verified { user, number } => {
                if super::super::safe_id(&user) && user != self.saved.user_id {
                    match number {
                        Some(number) => self.notes.verified.insert(user, number),
                        None => self.notes.verified.remove(&user),
                    };
                    self.save_notes();
                }
                Vec::new()
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn take_part(&mut self, conv: String, id: String, hash: String, part: u32, total: u32, data: String, from: &str) -> Vec<String> {
        if !super::super::safe_id(&conv) || !super::super::safe_id(&id) || self.is_self(&conv)
            || total == 0 || total as usize > MAX_FRAGMENTS || part >= total
            || hash.len() > 44 || data.len() > FRAGMENT_BYTES.div_ceil(3) * 4
        {
            return Vec::new();
        }
        let Some(expected) = super::unb64(&hash).filter(|hash| hash.len() == 32) else {
            return Vec::new();
        };
        let Some(bytes) = super::unb64(&data).filter(|bytes| bytes.len() <= FRAGMENT_BYTES) else {
            return Vec::new();
        };
        if self.cache(&conv).is_none_or(|cache| cache.rows.contains_key(&id)) {
            return Vec::new();
        }
        let now = Instant::now();
        self.siblings.parts.retain(|_, held| now.duration_since(held.at) < ASK_AGAIN);
        let key = (from.to_owned(), conv.clone(), id.clone(), hash);
        if !self.siblings.parts.contains_key(&key) && self.siblings.parts.len() >= MAX_ASSEMBLIES {
            return Vec::new();
        }
        let held = self.siblings.parts.entry(key.clone()).or_insert_with(|| Parts {
            at: now, chunks: vec![None; total as usize],
        });
        if held.chunks.len() != total as usize {
            return Vec::new();
        }
        if held.chunks[part as usize].as_ref().is_some_and(|before| *before != bytes) {
            self.siblings.parts.remove(&key);
            return Vec::new();
        }
        held.chunks[part as usize] = Some(bytes);
        if held.chunks.iter().any(Option::is_none) {
            return Vec::new();
        }
        let held = self.siblings.parts.remove(&key).unwrap();
        let bytes: Vec<u8> = held.chunks.into_iter().flatten().flatten().collect();
        if Sha256::digest(&bytes).as_slice() != expected {
            return Vec::new();
        }
        let Ok(opened) = serde_json::from_slice::<Opened>(&bytes) else {
            return Vec::new();
        };
        self.act(Sync::Have { conv, rows: vec![(id, opened)] }, from)
    }

    /// What this device can answer to a sibling's `want`: the asked messages its cache holds.
    fn answers_for(&mut self, conv: &str, ids: &[String]) -> Vec<Sync> {
        let rows: Vec<(String, Opened)> = match self.cache(conv) {
            Some(cache) => ids
                .iter()
                .take(ASK_AT_ONCE)
                .filter_map(|id| {
                    cache
                        .rows
                        .get(id)
                        .map(|opened| (id.clone(), opened.clone()))
                })
                .collect(),
            None => Vec::new(),
        };
        answers(conv, rows)
    }

    /// On start: the person's own conversation is made or joined, and what the siblings said
    /// while this device was away is acted on. Answers the conversations that gained messages.
    pub(crate) fn catch_up(&mut self) -> Vec<String> {
        let Some(conv) = self.self_conversation() else {
            return Vec::new();
        };
        if let Err(why) = self.sync(&conv) {
            tracing::warn!("chatd: Tiny e2ee: own conversation: {why}");
        }
        let rows = match self.ask(
            "GET",
            &format!("/v1/conversations/{conv}/messages?limit={CATCH_UP}"),
            None,
        ) {
            Ok((200, body)) => body
                .get("messages")
                .cloned()
                .unwrap_or(body)
                .as_array()
                .cloned()
                .unwrap_or_default(),
            _ => return Vec::new(),
        };
        let mut gained = BTreeSet::new();
        // The server gives the newest page oldest first: in the order they were said.
        for row in &rows {
            let text = |name: &str| row.get(name).and_then(Value::as_str);
            let (Some(id), Some(author_id)) = (text("id"), text("author_id")) else {
                continue;
            };
            let wire = Row {
                id,
                author_id,
                kind: text("kind").unwrap_or("message"),
                ciphertext: text("ciphertext"),
                epoch: row.get("epoch").and_then(Value::as_u64).unwrap_or(0),
                device_id: text("device_id").unwrap_or(""),
                target_id: text("target_id"),
                client_id: text("client_id"),
                created_at: text("created_at").unwrap_or(""),
            };
            gained.extend(self.sibling_said(&wire));
        }
        gained.into_iter().collect()
    }
}

/// After a page was read: asks the siblings for what did not open, off the reader's thread.
/// A device not yet in its own group (the server signs a new device's Add only after
/// `e2ee_commit_batch_secs`, 30 by default, and only when someone next asks) tries again a few
/// times, asking the server about the group first.
pub(crate) fn ask_soon(device: Shared) {
    if !lock(&device).has_wants() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("tiny-e2ee-siblings".into())
        .spawn(move || {
            for attempt in 0..RETRIES {
                if attempt > 0 {
                    std::thread::sleep(RETRY_AFTER);
                }
                let mut device = lock(&device);
                if let Some(own) = device.notes.self_conv.clone() {
                    let _ = device.sync(&own);
                }
                device.ask_siblings();
                if !device.has_wants() {
                    return;
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::super::{private_dir, Saved};
    use super::*;
    use e2ee::store::CacheKey;
    use e2ee::{Engine, UserId};

    const ME: &str = "7e57ada0-0000-4000-8000-0000000000c1";

    fn device(name: &str) -> Device {
        let saved = Saved {
            base_url: "http://127.0.0.1:9".into(),
            token: "t".into(),
            user_id: ME.into(),
            email: None,
            display_name: String::new(),
            role: "member".into(),
            plain_http_ok: false,
            e2ee: None,
        };
        let mut device = Device::new(&saved);
        device.dir =
            std::env::temp_dir().join(format!("chatd-e2ee-siblings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&device.dir);
        private_dir(&device.dir).unwrap();
        device.cache_key = Some(CacheKey::generate().unwrap());
        device.engine = Some(Engine::detached(UserId::parse(ME).unwrap()).unwrap());
        device.notes.self_conv = Some("5e1f0000-0000-4000-8000-000000000001".into());
        device
    }

    #[test]
    fn a_sync_row_that_becomes_readable_is_retried_instead_of_permanently_skipped() {
        let mut device = device("retry-readable");
        let own = device.notes.self_conv.clone().unwrap();
        let row = Row { id: "retry-row", author_id: ME, kind: "message", ciphertext: Some("AAAA"),
            epoch: 0, device_id: "sibling", target_id: None, client_id: None, created_at: "" };
        assert!(device.sibling_said(&row).is_empty());
        // Once a Welcome or another successful read makes this row available, catch-up must
        // still process its authenticated cached plaintext.
        let answer = Sync::Have { conv: "c1".into(), rows: vec![("m1".into(), opened("recovered"))] };
        device.remember(&own, row.id, opened(&answer.text().unwrap()));
        assert_eq!(device.sibling_said(&row), ["c1"]);
        assert_eq!(device.cache("c1").unwrap().rows["m1"].text, "recovered");
        assert!(device.sibling_said(&row).is_empty(), "a successfully handled row is deduplicated");
    }

    #[test]
    fn gap_fill_transfers_a_valid_message_larger_than_ten_kibibytes() {
        let text = "x".repeat(15_000);
        assert!(Envelope::message(&text).encode(Kind::Message).is_ok());
        let replies = answers("c1", vec![("large".into(), opened(&text))]);
        assert!(!replies.is_empty(), "a legal message must not disappear from gap fill");
        let mut receiver = device("large-answer");
        for reply in replies.into_iter().rev() {
            assert!(Envelope::message(reply.text().unwrap()).encode(Kind::Message).is_ok());
            receiver.act(reply, "sibling");
        }
        assert_eq!(receiver.cache("c1").unwrap().rows["large"].text, text);
    }

    #[test]
    fn gap_fill_measures_the_outer_envelope_when_text_needs_json_escaping() {
        let text = "\"".repeat(4_500);
        assert!(Envelope::message(&text).encode(Kind::Message).is_ok());
        let replies = answers("c1", vec![("quotes".into(), opened(&text))]);
        assert!(!replies.is_empty());
        let mut receiver = device("escaped-answer");
        for reply in replies {
            assert!(Envelope::message(reply.text().unwrap()).encode(Kind::Message).is_ok(), "the wire envelope must fit after both layers of escaping");
            receiver.act(reply, "sibling");
        }
        assert_eq!(receiver.cache("c1").unwrap().rows["quotes"].text, text);
    }

    #[test]
    fn fragmented_answers_are_authenticated_and_never_mix_devices() {
        let text = "\"".repeat(7_000);
        let replies = answers("c1", vec![("large".into(), opened(&text))]);
        assert!(replies.len() > 1, "large rows use independently bounded fragments");
        let mut receiver = device("separate-fragments");
        receiver.act(replies[0].clone(), "first-device");
        for reply in replies.iter().skip(1) {
            receiver.act(reply.clone(), "second-device");
        }
        assert!(!receiver.cache("c1").unwrap().rows.contains_key("large"));
        for reply in replies.iter().skip(1) {
            receiver.act(reply.clone(), "first-device");
        }
        assert_eq!(receiver.cache("c1").unwrap().rows["large"].text, text);

        let mut receiver = device("corrupt-fragments");
        let mut corrupt = serde_json::to_value(&replies[0]).unwrap();
        corrupt["data"] = serde_json::json!("AAAA");
        receiver.act(serde_json::from_value(corrupt).unwrap(), "sibling");
        for reply in replies.iter().skip(1) {
            receiver.act(reply.clone(), "sibling");
        }
        assert!(!receiver.cache("c1").unwrap().rows.contains_key("large"), "a bad assembled hash never persists a partial row");
        let mut oversized = serde_json::to_value(&replies[0]).unwrap();
        oversized["total"] = serde_json::json!(u32::MAX);
        receiver.act(serde_json::from_value(oversized).unwrap(), "sibling");
        for reply in replies {
            receiver.act(reply, "sibling");
        }
        assert_eq!(receiver.cache("c1").unwrap().rows["large"].text, text, "invalid fragment counts do not prevent a later valid retry");
    }

    #[test]
    fn a_sibling_answers_what_its_cache_holds_and_the_asker_keeps_it_without_overwriting() {
        let mut had = device("had");
        had.remember("c1", "m1", opened("opened there"));
        had.remember("c1", "m2", opened("also there"));
        let answers = had.answers_for("c1", &["m1".into(), "m2".into(), "m9".into()]);
        assert_eq!(answers.len(), 1);

        let mut missed = device("missed");
        missed.remember("c1", "m2", opened("already here"));
        let mut gained = Vec::new();
        for answer in answers {
            gained.extend(missed.act(answer, "sibling-device"));
        }
        assert_eq!(
            gained,
            vec!["c1".to_owned()],
            "the conversation is read again"
        );
        let rows = &missed.cache("c1").unwrap().rows;
        assert_eq!(rows["m1"].text, "opened there");
        assert_eq!(rows["m1"].imported_from.as_deref(), Some("sibling-device"));
        assert_eq!(rows["m2"].text, "already here", "never overwritten");
        // The same answer again changes nothing.
        let again = had.answers_for("c1", &["m1".into()]);
        assert!(missed
            .act(again.into_iter().next().unwrap(), "sibling-device")
            .is_empty());
        // Nothing is answered or kept for the person's own conversation itself.
        let own = missed.notes.self_conv.clone().unwrap();
        assert!(missed
            .act(
                Sync::Have {
                    conv: own.clone(),
                    rows: vec![("x".into(), opened("x"))]
                },
                "d"
            )
            .is_empty());
        let _ = std::fs::remove_dir_all(&had.dir);
        let _ = std::fs::remove_dir_all(&missed.dir);
    }

    #[test]
    fn a_verified_mark_follows_the_person_to_each_device() {
        let mut device = device("verified");
        device.act(
            Sync::Verified {
                user: "b0b00000-0000-4000-8000-000000000002".into(),
                number: Some("123".into()),
            },
            "d",
        );
        assert_eq!(
            device
                .notes
                .verified
                .get("b0b00000-0000-4000-8000-000000000002")
                .map(String::as_str),
            Some("123")
        );
        device.act(
            Sync::Verified {
                user: "b0b00000-0000-4000-8000-000000000002".into(),
                number: None,
            },
            "d",
        );
        assert!(device.notes.verified.is_empty());
        // A mark on the person themself is never taken.
        device.act(
            Sync::Verified {
                user: ME.into(),
                number: Some("1".into()),
            },
            "d",
        );
        assert!(device.notes.verified.is_empty());
        let _ = std::fs::remove_dir_all(&device.dir);
    }

    #[test]
    fn only_the_persons_other_devices_are_listened_to() {
        let mut device = device("listen");
        let mine = device.engine.as_ref().unwrap().device_id().to_string();
        let own = device.notes.self_conv.clone().unwrap();
        let row = |id: &'static str, author: &'static str, from: &'static str| Row {
            id,
            author_id: author,
            kind: "message",
            ciphertext: Some("AAAA"),
            epoch: 0,
            device_id: from,
            target_id: None,
            client_id: None,
            created_at: "",
        };
        assert!(device
            .sibling_said(&row("r1", "b0b00000-0000-4000-8000-000000000002", "other"))
            .is_empty());
        let mine: &'static str = Box::leak(mine.into_boxed_str());
        assert!(device.sibling_said(&row("r2", ME, mine)).is_empty());
        // One that does not open remains retryable after its Welcome arrives.
        assert!(device.sibling_said(&row("r3", ME, "sibling")).is_empty());
        assert!(!device.siblings.seen.contains("r3"));
        assert!(
            !device.has_wants(),
            "a sync message is never itself asked for"
        );
        let _ = own;
        let _ = std::fs::remove_dir_all(&device.dir);
    }

    #[test]
    fn what_did_not_open_is_asked_once_in_a_while() {
        let mut device = device("asks");
        device.self_capable = true;
        device.want("c1", vec!["m1".into(), "m2".into()]);
        device.want("c1", vec!["m1".into(), "m3".into()]);
        assert_eq!(
            device.siblings.wanted,
            vec![
                ("c1".to_owned(), vec!["m1".to_owned(), "m2".to_owned()]),
                ("c1".to_owned(), vec!["m3".to_owned()])
            ]
        );
        let own = device.notes.self_conv.clone().unwrap();
        device.want(&own, vec!["x".into()]);
        assert_eq!(
            device.siblings.wanted.len(),
            2,
            "the person's own conversation is never asked about"
        );
        // Not in its own group yet (here: no server at all): nothing is lost, it is asked again.
        device.ask_siblings();
        assert_eq!(device.siblings.wanted.len(), 2);
        let _ = std::fs::remove_dir_all(&device.dir);
    }

    fn opened(text: &str) -> Opened {
        Opened {
            text: text.into(),
            at: 7,
            ..Opened::default()
        }
    }

    #[test]
    fn a_sync_message_reads_back_and_ordinary_text_is_not_one() {
        for sync in [
            Sync::Want {
                conv: "c".into(),
                ids: vec!["a".into(), "b".into()],
            },
            Sync::Have {
                conv: "c".into(),
                rows: vec![("a".into(), opened("hi"))],
            },
            Sync::Verified {
                user: "u".into(),
                number: Some("123".into()),
            },
            Sync::Verified {
                user: "u".into(),
                number: None,
            },
        ] {
            assert!(Sync::parse(&sync.text().unwrap()) == Some(sync));
        }
        assert!(Sync::parse("hello").is_none());
        assert!(Sync::parse("tiny-sync-1 {\"t\":\"steal\"}").is_none());
        assert!(Sync::parse("tiny-sync-2 {\"t\":\"want\",\"conv\":\"c\",\"ids\":[]}").is_none());
    }

    #[test]
    fn answers_fit_one_envelope_each_and_keep_every_row_that_fits() {
        let rows: Vec<(String, Opened)> = (0..60)
            .map(|n| (format!("m{n}"), opened(&"x".repeat(900))))
            .collect();
        let mut too_big = vec![("huge".to_owned(), opened(&"y".repeat(MAX_ROW_BYTES + 1)))];
        too_big.extend(rows.clone());
        let out = answers("c1", too_big);
        assert!(out.len() > 1);
        let mut kept = 0;
        for answer in &out {
            let text = answer.text().unwrap();
            assert!(Envelope::message(text).encode(Kind::Message).is_ok());
            let Sync::Have { rows, .. } = answer else {
                panic!()
            };
            assert!(
                rows.iter().all(|(id, _)| id != "huge"),
                "too large for any message"
            );
            kept += rows.len();
        }
        assert_eq!(kept, 60);
        assert!(answers("c1", Vec::new()).is_empty());
    }
}

/// Gap fill against a real tinyserver with `e2ee_self`: two sessions of one person in
/// `TINY_SELF_SESSIONS` (a JSON array) with no device registered yet. The first device is
/// trusted on first use and approves the second; both join the person's own conversation; the
/// second asks for a message only the first has, and gets it. Both are revoked at the end.
///
/// `cargo test -p chatd --no-default-features --lib siblings::live -- --ignored --nocapture`
#[cfg(test)]
mod live {
    use super::super::{lock, private_dir, DeviceState, Saved, Secrets};
    use super::*;
    use std::sync::{Arc, Mutex};

    fn make(saved: &Saved, name: &str) -> Shared {
        let mut saved = saved.clone();
        saved.e2ee = Some(Secrets::generate().unwrap());
        let mut device = Device::new(&saved);
        device.dir = std::env::temp_dir().join(format!(
            "chatd-e2ee-self-live-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&device.dir);
        private_dir(&device.dir).unwrap();
        Arc::new(Mutex::new(device))
    }

    #[test]
    #[ignore]
    fn live_a_new_device_gets_what_it_cannot_open_from_its_sibling() {
        let Ok(text) = std::env::var("TINY_SELF_SESSIONS") else {
            eprintln!("TINY_SELF_SESSIONS is not set");
            return;
        };
        let rows: Vec<Value> = serde_json::from_str(&text).unwrap();
        let sessions: Vec<Saved> = rows
            .iter()
            .map(|row| crate::adapters::tiny::parse(&row.to_string()).unwrap())
            .collect();
        let (first, second) = (make(&sessions[0], "first"), make(&sessions[1], "second"));
        if !lock(&first).list(&sessions[0].user_id).unwrap().is_empty() {
            eprintln!("this person already has devices: not touching them");
            return;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lock(&first).enrol();
            assert_eq!(lock(&first).state, DeviceState::Ready);
            assert!(lock(&first).self_capable, "the server has e2ee_self");
            lock(&second).enrol();
            let DeviceState::AwaitingApproval { code } = lock(&second).state.clone() else {
                panic!("the second device waits to be approved");
            };
            lock(&first).approve(&code).unwrap();
            lock(&second).enrol();
            assert_eq!(lock(&second).state, DeviceState::Ready);

            // Both in the person's own conversation: the first starts its group and commits the
            // second's Add; the second joins from the Welcome.
            let own = lock(&first)
                .self_conversation()
                .expect("the person's own conversation");
            assert_eq!(
                lock(&second).self_conversation().as_deref(),
                Some(own.as_str()),
                "one per person"
            );
            // The server signs the second device's Add only after its commit batching (30 s by
            // default), when someone next asks; the first device then commits it.
            let joined = (0..20).any(|_| {
                lock(&first).sync(&own).unwrap();
                let _ = lock(&second).sync(&own);
                let in_group = lock(&second).engine.as_mut().is_some_and(|engine| {
                    engine
                        .epoch(e2ee::ConversationId(super::super::uuid(&own).unwrap()))
                        .is_ok()
                });
                if !in_group {
                    std::thread::sleep(Duration::from_secs(5));
                }
                in_group
            });
            assert!(joined, "the second device joined the person's own group");

            let conv = "00000000-0000-4000-8000-0000000c0de1";
            lock(&first).remember(
                conv,
                "00000000-0000-4000-8000-00000000c0de",
                Opened {
                    text: "only the first opened this".into(),
                    at: 1,
                    ..Opened::default()
                },
            );
            lock(&second).want(conv, vec!["00000000-0000-4000-8000-00000000c0de".into()]);
            lock(&second).ask_siblings();
            let _ = lock(&first).catch_up();
            let gained = lock(&second).catch_up();
            assert_eq!(gained, vec![conv.to_owned()]);
            let mut second = lock(&second);
            let kept = second.cache(conv).and_then(|cache| {
                cache
                    .rows
                    .get("00000000-0000-4000-8000-00000000c0de")
                    .cloned()
            });
            assert_eq!(
                kept.map(|opened| opened.text).as_deref(),
                Some("only the first opened this")
            );
        }));
        for device in [&second, &first] {
            let device = lock(device);
            if let Some(engine) = device.engine.as_ref() {
                let _ = device.ask(
                    "DELETE",
                    &format!("/v1/e2ee/devices/{}", engine.device_id()),
                    None,
                );
            }
            let _ = std::fs::remove_dir_all(&device.dir);
        }
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
