//! Conversation-generation discovery, fencing and explicitly approved repair.

use super::*;
use e2ee::repair::{Binding, GroupGeneration};

const UNCHECKED: &str =
    "This conversation's encryption update could not be verified. Try again when connected.";
const UNSUPPORTED: &str = "This server cannot safely continue this encryption update.";
const UNAVAILABLE: &str = "This chat cannot update its encryption yet.";

fn outer(id: &str) -> Result<ConversationId, String> {
    ConversationId::parse(id).map_err(|_| super::super::CHECK.to_owned())
}

fn hash(value: &Value) -> Option<[u8; 32]> {
    let text = value.as_str()?;
    let bytes = unb64(text)?;
    (b64(&bytes) == text).then_some(bytes)?.try_into().ok()
}

/// The independent sibling decoder must distinguish missing legacy metadata from malformed
/// modern metadata. `as_u64().unwrap_or(0)` would silently turn a negative generation into zero.
pub(super) fn row_metadata(row: &Value) -> Result<(Option<u64>, Option<&str>), String> {
    let generation = row
        .get("generation")
        .map(|value| {
            value
                .as_u64()
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or(UNCHECKED)
        })
        .transpose()?;
    let group = row
        .get("group_id")
        .map(|value| {
            value
                .as_str()
                .filter(|id| ConversationId::parse(id).is_ok())
                .ok_or(UNCHECKED)
        })
        .transpose()?;
    Ok((generation, group))
}

impl Device {
    pub(crate) fn react_generation(
        &mut self,
        id: &str,
        message: &str,
        emoji: &str,
        on: bool,
    ) -> Result<(), String> {
        for _ in 0..PASSES {
            self.sync(id)?;
            let active = self.active(id)?;
            let mut body = json!({ "emoji": emoji });
            self.generation_body(&mut body, active.generation);
            let (status, answer) = self.ask(
                if on { "PUT" } else { "DELETE" },
                &format!("/v1/messages/{message}/reactions"),
                Some(body),
            )?;
            if matches!(status, 200 | 201 | 204) {
                return Ok(());
            }
            if status == 409 && answer["error"] == "generation_changed" {
                self.discover_generations(id)?;
                continue;
            }
            return Err(
                super::super::explain(status, &answer, super::super::Missing::Plain).to_owned(),
            );
        }
        Err("Encryption is still updating. Try again shortly.".to_owned())
    }

    pub(super) fn active(&mut self, id: &str) -> Result<GroupGeneration, String> {
        let conversation = outer(id)?;
        if self.generations
            && matches!(
                self.engine()?.active_group(conversation),
                Err(Error::RepairPending)
            )
        {
            // An observed first bootstrap did not finish publishing locally. Recover only
            // through the immutable chain and exact engine bootstrap retry; never emit a
            // legacy-shaped request or synthesize an active write generation.
            self.discover_generations(id)?;
        }
        let engine = self.engine()?;
        let active = engine
            .active_group(conversation)
            .map_err(|err| words(&err))?;
        let modern = active.generation > 0 || engine.prepared_repair(conversation).is_some();
        if modern && !self.generations {
            return Err(UNSUPPORTED.to_owned());
        }
        Ok(active)
    }

    pub(super) fn generation_query(&self, path: String, generation: u64) -> String {
        if self.generations {
            let join = if path.contains('?') { '&' } else { '?' };
            format!("{path}{join}generation={generation}")
        } else {
            path
        }
    }

    pub(super) fn generation_body(&self, body: &mut Value, generation: u64) {
        if self.generations {
            body["generation"] = json!(generation);
        }
    }

    /// A server head is a query hint until the signed chain has been accepted by the engine.
    pub(super) fn group_info(&mut self, id: &str) -> Result<(GroupGeneration, Value), String> {
        for _ in 0..2 {
            let active = self.active(id)?;
            let path =
                self.generation_query(format!("/v1/e2ee/conversations/{id}"), active.generation);
            let (status, info) = self.ask("GET", &path, None)?;
            if self.generations && status == 409 && info["error"] == "generation_changed" {
                self.discover_generations(id)?;
                continue;
            }
            if status != 200 {
                return Err(
                    super::super::explain(status, &info, super::super::Missing::Plain).to_owned(),
                );
            }
            if info["e2ee"] == true {
                let key = info["external_sender"]
                    .as_str()
                    .and_then(unb64)
                    .ok_or(UNCHECKED)?;
                self.check_generation_info(id, &active, &info, &key)?;
            } else if active.head_hash.is_some() || active.generation > 0 {
                return Err(UNCHECKED.to_owned());
            }
            return Ok((active, info));
        }
        Err(UNCHECKED.to_owned())
    }

    fn check_generation_info(
        &self,
        id: &str,
        active: &GroupGeneration,
        info: &Value,
        key: &[u8],
    ) -> Result<(), String> {
        if !self.generations {
            return Ok(());
        }
        let generation = info["generation"].as_u64().ok_or(UNCHECKED)?;
        let group = info["group_id"]
            .as_str()
            .and_then(|id| ConversationId::parse(id).ok())
            .ok_or(UNCHECKED)?;
        let head = hash(&info["head_hash"]).ok_or(UNCHECKED)?;
        if generation != active.generation {
            return Err(UNCHECKED.to_owned());
        }
        // Shared generation-zero groups keep the existing adapter's routing. Repair refuses
        // these aliases; introducing alias mappings is a separate migration.
        if generation == 0 && group != outer(id)? {
            return Ok(());
        }
        let expected = if generation == 0 {
            e2ee::repair::genesis_hash(outer(id)?, key).map_err(|err| words(&err))?
        } else {
            active.head_hash.ok_or(UNCHECKED)?
        };
        if group != active.group_id
            || head != expected
            || active.head_hash.is_some_and(|known| known != head)
        {
            return Err(UNCHECKED.to_owned());
        }
        Ok(())
    }

    fn fresh_signer(&mut self, binding: &Binding) -> Result<(), String> {
        let user = binding.user_id.to_string();
        let rows = self.list(&user)?;
        let present = rows.iter().any(|row| {
            row["device_id"].as_str() == Some(binding.device_id.to_string().as_str())
                && row["signature_key"].as_str().and_then(unb64).as_deref()
                    == Some(binding.signature_key.as_slice())
        });
        if !present {
            return Err(UNCHECKED.to_owned());
        }
        self.verify_rows(&user, &rows)?;
        if !self.engine()?.trust().is_trusted(
            binding.user_id,
            binding.device_id,
            &binding.signature_key,
        ) {
            return Err(UNCHECKED.to_owned());
        }
        Ok(())
    }

    pub(super) fn discover_generations(&mut self, id: &str) -> Result<(), String> {
        if !self.generations {
            return Err(UNSUPPORTED.to_owned());
        }
        let conversation = outer(id)?;
        for _ in 0..PASSES {
            let active = match self.engine()?.active_group(conversation) {
                Ok(active) => active,
                // Engine reserves this error for an unpublished first-bootstrap successor.
                // This zero is only the immutable chain query's starting point, never a
                // routing result. bootstrap_repair authoritatively permits the exact retry.
                Err(Error::RepairPending) => GroupGeneration {
                    generation: 0,
                    group_id: conversation,
                    head_hash: None,
                },
                Err(err) => return Err(words(&err)),
            };
            let (status, chain) = self.ask(
                "GET",
                &format!(
                    "/v1/e2ee/conversations/{id}/generations?after={}&limit=100",
                    active.generation
                ),
                None,
            )?;
            if status != 200 {
                return Err(UNCHECKED.to_owned());
            }
            let head = chain["generation"]
                .as_u64()
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or(UNCHECKED)?;
            let group = chain["group_id"]
                .as_str()
                .and_then(|id| ConversationId::parse(id).ok())
                .ok_or(UNCHECKED)?;
            let head_hash = hash(&chain["head_hash"]).ok_or(UNCHECKED)?;
            if head < active.generation {
                return Err(UNCHECKED.to_owned());
            }
            let rows = chain["bindings"]
                .as_array()
                .filter(|rows| rows.len() <= 100)
                .ok_or(UNCHECKED)?;
            if head == active.generation {
                if !rows.is_empty()
                    || active.group_id != group
                    || active.head_hash != Some(head_hash)
                {
                    return Err(UNCHECKED.to_owned());
                }
                return Ok(());
            }
            if rows.is_empty() {
                return Err(UNCHECKED.to_owned());
            }
            let mut bindings: Vec<Binding> = rows
                .iter()
                .cloned()
                .map(serde_json::from_value)
                .collect::<Result<_, _>>()
                .map_err(|_| UNCHECKED)?;
            let unknown = active.head_hash.is_none();
            if unknown {
                for _ in 1..PASSES {
                    let after = bindings.last().ok_or(UNCHECKED)?.to_generation;
                    if after >= head {
                        break;
                    }
                    let (status, page) = self.ask(
                        "GET",
                        &format!("/v1/e2ee/conversations/{id}/generations?after={after}&limit=100"),
                        None,
                    )?;
                    if status != 200
                        || page["generation"].as_u64() != Some(head)
                        || page["group_id"].as_str() != Some(group.to_string().as_str())
                        || hash(&page["head_hash"]) != Some(head_hash)
                    {
                        return Err(UNCHECKED.to_owned());
                    }
                    let rows = page["bindings"]
                        .as_array()
                        .filter(|rows| !rows.is_empty() && rows.len() <= 100)
                        .ok_or(UNCHECKED)?;
                    let more: Vec<Binding> = rows
                        .iter()
                        .cloned()
                        .map(serde_json::from_value)
                        .collect::<Result<_, _>>()
                        .map_err(|_| UNCHECKED)?;
                    if more
                        .last()
                        .is_none_or(|binding| binding.to_generation <= after)
                    {
                        return Err(UNCHECKED.to_owned());
                    }
                    bindings.extend(more);
                }
            }
            // A never-accepted device obtains the external key independently of the binding.
            let key = if unknown {
                let (status, info) = self.ask(
                    "GET",
                    &format!("/v1/e2ee/conversations/{id}?generation={head}"),
                    None,
                )?;
                if status != 200
                    || info["e2ee"] != true
                    || info["generation"].as_u64() != Some(head)
                    || info["group_id"].as_str() != Some(group.to_string().as_str())
                    || hash(&info["head_hash"]) != Some(head_hash)
                {
                    return Err(UNCHECKED.to_owned());
                }
                Some(
                    info["external_sender"]
                        .as_str()
                        .and_then(unb64)
                        .ok_or(UNCHECKED)?,
                )
            } else {
                None
            };
            let mut previous = active
                .head_hash
                .or_else(|| {
                    key.as_deref()
                        .and_then(|key| e2ee::repair::genesis_hash(conversation, key).ok())
                })
                .ok_or(UNCHECKED)?;
            let mut generation = active.generation;
            let mut internal = active.group_id;
            for binding in &bindings {
                binding.verify_signature().map_err(|_| UNCHECKED)?;
                if binding.conversation_id != conversation
                    || binding.from_generation != generation
                    || binding.to_generation > head
                    || binding.from_group_id != internal
                    || binding.previous_hash != previous
                {
                    return Err(UNCHECKED.to_owned());
                }
                if unknown
                    && bindings
                        .last()
                        .is_some_and(|last| binding.server_key_hash != last.server_key_hash)
                {
                    return Err(UNCHECKED.to_owned());
                }
                generation = binding.to_generation;
                internal = binding.group_id;
                previous = binding.payload_hash().map_err(|_| UNCHECKED)?;
            }
            if generation == head && (internal != group || previous != head_hash) {
                return Err(UNCHECKED.to_owned());
            }
            if unknown {
                // The anchored historical chain was structurally/signature checked above.
                // A first bootstrap independently approves its current terminal signer;
                // ordinary revocation of historical signers must not prevent a new link.
                self.fresh_signer(bindings.last().ok_or(UNCHECKED)?)?;
            } else {
                let mut checked = BTreeSet::new();
                for binding in &bindings {
                    if checked.insert((
                        binding.user_id,
                        binding.device_id,
                        binding.signature_key.clone(),
                    )) {
                        self.fresh_signer(binding)?;
                    }
                }
            }
            if !unknown {
                for binding in &bindings {
                    // A device that missed several generations must authenticate each old
                    // roster. Its ordinary historical Welcome/feed may supply the intermediate
                    // state; absence never turns into the never-accepted bootstrap path.
                    if !self.engine()?.has_group(binding.from_group_id) {
                        let key = self
                            .engine()?
                            .server_key(binding.from_group_id)
                            .ok_or(UNCHECKED)?;
                        self.feed_at(id, binding.from_group_id, binding.from_generation, &key)?;
                    }
                    self.engine()?
                        .accept_repair(binding)
                        .map_err(|err| words(&err))?;
                }
            }
            if unknown {
                // Bootstrap has no authenticated old roster. Require the complete anchored chain
                // in this bounded response, never silently skip a page of bindings.
                if generation != head || internal != group || previous != head_hash {
                    return Err(UNCHECKED.to_owned());
                }
                self.engine()?
                    .bootstrap_repair(
                        bindings.last().ok_or(UNCHECKED)?,
                        key.as_deref().ok_or(UNCHECKED)?,
                    )
                    .map_err(|err| words(&err))?;
            }
            if generation == head {
                if internal != group || previous != head_hash {
                    return Err(UNCHECKED.to_owned());
                }
                return Ok(());
            }
        }
        Err(UNCHECKED.to_owned())
    }

    pub(super) fn row_group(&mut self, id: &str, row: &Row) -> Result<ConversationId, String> {
        let active = self
            .engine()?
            .active_group(outer(id)?)
            .map_err(|_| UNCHECKED)?;
        let generation = row.generation.unwrap_or(0);
        if generation > active.generation {
            self.discover_generations(id)?;
        }
        let internal = self
            .engine()?
            .group_at(outer(id)?, generation)
            .map_err(|_| UNCHECKED)?;
        if generation > 0 || row.generation.is_some() {
            let group = row
                .group_id
                .and_then(|id| ConversationId::parse(id).ok())
                .ok_or(UNCHECKED)?;
            if group != internal && generation > 0 {
                return Err(UNCHECKED.to_owned());
            }
        }
        Ok(internal)
    }
}

struct Job {
    state: Mutex<protocol::E2eeRepair>,
    told: linking::Told,
}

fn jobs() -> &'static Mutex<HashMap<(String, String), Arc<Job>>> {
    static JOBS: OnceLock<Mutex<HashMap<(String, String), Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

fn key(saved: &Saved) -> (String, String) {
    (saved.base_url.clone(), saved.user_id.clone())
}

pub(super) fn view(saved: &Saved) -> Option<protocol::E2eeRepair> {
    let job = lock(jobs()).get(&key(saved)).cloned()?;
    let view = lock(&job.state).clone();
    Some(view)
}

impl Job {
    fn stage(&self, stage: protocol::RepairStage, error: String) {
        {
            let mut state = lock(&self.state);
            state.stage = stage;
            state.error = error;
        }
        (self.told)();
    }
}

pub(super) fn start(saved: &Saved, id: &str, told: linking::Told) -> Result<(), String> {
    start_with(saved, id, told, approval::service())
}

fn start_with(
    saved: &Saved,
    id: &str,
    told: linking::Told,
    verifier: Arc<dyn protocol::device_approval::DeviceApproval>,
) -> Result<(), String> {
    outer(id)?;
    // Queue immediately even when an ordinary sync currently owns the device lock.
    let shared = started(saved).ok_or("Encryption isn't set up on this device.")?;
    let job = Arc::new(Job {
        state: Mutex::new(protocol::E2eeRepair {
            conversation_id: id.to_owned(),
            stage: protocol::RepairStage::Approving,
            error: String::new(),
        }),
        told,
    });
    {
        let mut jobs = lock(jobs());
        if jobs.get(&key(saved)).is_some_and(|job| {
            !matches!(
                lock(&job.state).stage,
                protocol::RepairStage::Done | protocol::RepairStage::Failed
            )
        }) {
            return Err("An encryption update is already running.".to_owned());
        }
        jobs.insert(key(saved), job.clone());
    }
    let saved = saved.clone();
    let id = id.to_owned();
    let thread_job = job.clone();
    let spawned = std::thread::Builder::new()
        .name("tiny-encryption-update".into())
        .spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            let result = run(
                &thread_job,
                &shared,
                &saved,
                &id,
                verifier.as_ref(),
                deadline,
            );
            match result {
                Ok(()) => thread_job.stage(protocol::RepairStage::Done, String::new()),
                Err(error) => thread_job.stage(protocol::RepairStage::Failed, error),
            }
        });
    if spawned.is_err() {
        job.stage(
            protocol::RepairStage::Failed,
            "The encryption update could not start. Try again.".to_owned(),
        );
        return Err("The encryption update could not start. Try again.".to_owned());
    }
    (job.told)();
    Ok(())
}

fn acquire(
    shared: &Shared,
    deadline: std::time::Instant,
) -> Result<std::sync::MutexGuard<'_, Device>, String> {
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("Encryption update timed out. Try again to continue safely.".to_owned());
        }
        match shared.try_lock() {
            Ok(device) => return Ok(device),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
        }
    }
}

fn run(
    job: &Job,
    shared: &Shared,
    saved: &Saved,
    id: &str,
    verifier: &dyn protocol::device_approval::DeviceApproval,
    deadline: std::time::Instant,
) -> Result<(), String> {
    let approval_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let current = || started(saved).is_some_and(|now| Arc::ptr_eq(&now, shared));
    let cancelled = || std::time::Instant::now() >= approval_deadline || !current();
    (|| {
        let identity = {
            let mut device = acquire(shared, deadline)?;
            if cancelled() || !device.ready() || !device.generations {
                return Err("This device cannot update encryption yet.".to_owned());
            }
            let identity = device.engine()?.device_id();
            device.operation_deadline = Some(deadline);
            let preflight = device.group_info(&id).and_then(|(active, info)| {
                if info["e2ee"] == true
                    && info["repair_available"] == true
                    && info["group_id"].as_str() == Some(active.group_id.to_string().as_str())
                {
                    Ok(())
                } else {
                    Err(UNAVAILABLE.to_owned())
                }
            });
            device.operation_deadline = None;
            preflight?;
            identity
        };
        if cancelled() {
            return Err("Encryption update was canceled.".to_owned());
        }
        approval::verify(
            verifier,
            "Approve updating encryption for this conversation",
            &cancelled,
        )?;
        if cancelled() {
            return Err("Encryption update was canceled.".to_owned());
        }
        job.stage(protocol::RepairStage::Updating, String::new());
        {
            let mut device = acquire(shared, deadline)?;
            if !current()
                || !device.ready()
                || device.engine()?.device_id() != identity
                || cancelled()
            {
                return Err("This device changed. Open Encryption and try again.".to_owned());
            }
            device.operation_deadline = Some(deadline);
            let result = device.submit_repair(&id);
            device.operation_deadline = None;
            result?;
        }
        job.stage(protocol::RepairStage::WaitingForDevices, String::new());
        loop {
            if !current() || std::time::Instant::now() >= deadline {
                return Err(
                    "The update is waiting for devices. Try again to continue safely.".to_owned(),
                );
            }
            let done = {
                let mut device = acquire(shared, deadline)?;
                device.operation_deadline = Some(deadline);
                let result = (|| {
                    device.sync(&id)?;
                    let (_, info) = device.group_info(&id)?;
                    let active = device.active(id)?;
                    let engine = device.engine()?;
                    let own = engine.device_ref();
                    let joined = engine.has_group(active.group_id)
                        && engine
                            .roster(active.group_id)
                            .map_err(|err| words(&err))?
                            .contains(&own);
                    let done = joined && info["pending"].as_array().is_none_or(Vec::is_empty);
                    if done {
                        if let Some(commands) = &device.commands {
                            let _ =
                                commands.send(crate::adapters::Command::Backfill(id.to_owned()));
                        }
                    }
                    Ok::<_, String>(done)
                })();
                device.operation_deadline = None;
                result?
            };
            if done {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    })()
}

impl Device {
    fn submit_repair(&mut self, id: &str) -> Result<(), String> {
        let conversation = outer(id)?;
        let attempted = self.engine()?.prepared_repair(conversation);
        let (active, info) = self.group_info(id)?;
        if attempted
            .as_ref()
            .is_some_and(|prepared| active.generation >= prepared.binding.to_generation)
        {
            // Discovery authenticated our response (or the winning competing successor).
            // Continue admitting devices instead of accidentally preparing another generation.
            return Ok(());
        }
        if !self.generations || info["e2ee"] != true {
            return Err(UNSUPPORTED.to_owned());
        }
        if info["repair_available"] != true {
            return Err(UNAVAILABLE.to_owned());
        }
        if info["group_id"].as_str() != Some(active.group_id.to_string().as_str()) {
            return Err(UNAVAILABLE.to_owned());
        }
        // The current ordinary directory must still approve this exact device. A binding
        // never introduces its own trust anchor.
        let own = self.engine()?.device_ref();
        self.verify_user(&own.user.to_string())?;
        if !self
            .engine()?
            .trust()
            .is_trusted(own.user, own.device, &own.signature_key)
            || !self
                .engine()?
                .roster(active.group_id)
                .map_err(|err| words(&err))?
                .contains(&own)
        {
            return Err("This device is not an approved member of this conversation.".to_owned());
        }
        // Retry the persisted candidate, including its exact signature and private state.
        let binding = match self.engine()?.prepared_repair(conversation) {
            Some(prepared) => prepared.binding,
            None => self
                .engine()?
                .prepare_repair(
                    conversation,
                    active.generation,
                    info["epoch"].as_u64().ok_or(UNCHECKED)?,
                )
                .map_err(|err| words(&err))?,
        };
        let binding_hash = binding.payload_hash().map_err(|err| words(&err))?;
        self.engine()?
            .mark_repair_submitted(conversation, binding_hash)
            .map_err(|err| words(&err))?;
        let (status, answer) = self.ask(
            "POST",
            &format!("/v1/e2ee/conversations/{id}/repair"),
            Some(json!({ "binding": binding })),
        )?;
        match status {
            200 | 201 => {
                let returned: Binding =
                    serde_json::from_value(answer["binding"].clone()).map_err(|_| UNCHECKED)?;
                if returned != binding
                    || answer["generation"].as_u64() != Some(binding.to_generation)
                    || answer["group_id"].as_str() != Some(binding.group_id.to_string().as_str())
                    || hash(&answer["head_hash"]) != Some(binding_hash)
                {
                    return Err(UNCHECKED.to_owned());
                }
                self.engine()?
                    .accept_repair(&binding)
                    .map_err(|err| words(&err))?;
                Ok(())
            }
            409 if answer["error"] == "generation_changed" => self.discover_generations(id),
            409 if answer["error"] == "repair_unavailable" => Err(UNAVAILABLE.to_owned()),
            _ => Err(
                "The encryption update could not finish. Try again to continue safely.".to_owned(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::{run, Job};

    const CONV: &str = "22222222-2222-2222-2222-222222222222";

    struct Remote {
        key: Vec<u8>,
        directory: Vec<Value>,
        bindings: Vec<e2ee::repair::Binding>,
        available: bool,
        corrupt_head: bool,
        messages: usize,
        refuse_posts: bool,
    }

    fn remote(device: &mut Device, requests: Arc<Mutex<Vec<String>>>) -> Arc<Mutex<Remote>> {
        let engine = device.engine().unwrap();
        let state = Arc::new(Mutex::new(Remote {
            key: engine
                .server_key(ConversationId::parse(CONV).unwrap())
                .unwrap(),
            directory: vec![
                json!({"device_id":engine.device_id().to_string(),"signature_key":b64(&engine.signature_key()),"created_at":"2026-01-01T00:00:00Z"}),
            ],
            bindings: Vec::new(),
            available: true,
            corrupt_head: false,
            messages: 0,
            refuse_posts: false,
        }));
        let serving = state.clone();
        device.saved.base_url = super::super::super::tests::serve(move |request| {
            lock(&requests).push(request.to_owned());
            let mut state = lock(&serving);
            let current = state.bindings.last();
            let generation = current.map_or(0, |binding| binding.to_generation);
            let group = current.map_or(ConversationId::parse(CONV).unwrap(), |binding| {
                binding.group_id
            });
            let head = current.map_or_else(
                || {
                    e2ee::repair::genesis_hash(ConversationId::parse(CONV).unwrap(), &state.key)
                        .unwrap()
                },
                |binding| binding.payload_hash().unwrap(),
            );
            let receipt = |binding: &e2ee::repair::Binding| json!({"generation":binding.to_generation,"group_id":binding.group_id.to_string(),"head_hash":b64(&binding.payload_hash().unwrap()),"binding":binding});
            let (status, body) = if request.contains("/repair ") {
                let body: Value =
                    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                let binding: e2ee::repair::Binding =
                    serde_json::from_value(body["binding"].clone()).unwrap();
                state.bindings.push(binding.clone());
                (201, receipt(&binding))
            } else if request.contains("/generations?") {
                (
                    200,
                    json!({"generation":generation,"group_id":group.to_string(),"head_hash":b64(&if state.corrupt_head { [0;32] } else { head }),"bindings":state.bindings}),
                )
            } else if request.contains("/handshake?") {
                (200, json!([]))
            } else if request.starts_with("GET /v1/e2ee/conversations/") {
                if !request
                    .lines()
                    .next()
                    .unwrap()
                    .contains(&format!("generation={generation} "))
                {
                    (409, json!({"error":"generation_changed"}))
                } else {
                    (
                        200,
                        json!({"e2ee":true,"epoch":0,"external_sender":b64(&state.key),"generation":generation,"group_id":group.to_string(),"head_hash":b64(&head),"repair_available":state.available,"pending":[]}),
                    )
                }
            } else if request.starts_with("GET /v1/e2ee/users/") {
                (200, json!(state.directory))
            } else if request.starts_with("POST /v1/conversations/") {
                state.messages += 1;
                if state.refuse_posts {
                    (503, json!({"error":"unavailable"}))
                } else {
                    (201, json!({"id":format!("message{}", state.messages)}))
                }
            } else if request.contains("/reactions ") {
                (204, json!({}))
            } else {
                (200, json!({}))
            };
            super::super::super::tests::http(status, "Test", &body.to_string())
        });
        state
    }

    fn modern_device(name: &str, has_group: bool) -> (Device, Arc<Mutex<Vec<String>>>) {
        let user = UserId::parse("11111111-1111-1111-1111-111111111111").unwrap();
        let key = e2ee::identity::DeviceKey::generate().unwrap().public_key();
        let dir =
            std::env::temp_dir().join(format!("tiny-generation-{}-{name}", std::process::id()));
        private_dir(&dir).unwrap();
        let mut engine = Engine::create(
            user,
            Box::new(FileStore::new(&dir)),
            StateKey::generate().unwrap(),
            STATE,
        )
        .unwrap();
        if has_group {
            engine
                .create_group(ConversationId::parse(CONV).unwrap(), &key)
                .unwrap();
        }
        let own = json!({"device_id":engine.device_id().to_string(),"signature_key":b64(&engine.signature_key()),"created_at":"2026-01-01T00:00:00Z"});
        let head = e2ee::repair::genesis_hash(ConversationId::parse(CONV).unwrap(), &key).unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let requests = captured.clone();
        let base = super::super::super::tests::serve(move |request| {
            lock(&requests).push(request.to_owned());
            let (status, body) = if request.starts_with("GET /v1/capabilities ") {
                (
                    200,
                    json!({"features":["e2ee_text","e2ee_group_generations"],"e2ee_text":"optional"}),
                )
            } else if request.starts_with("GET /v1/e2ee/users/") {
                (200, json!([own.clone()]))
            } else if request.starts_with("GET /v1/e2ee/key_packages/count?") {
                (200, json!({"remaining":50}))
            } else if request.contains("/handshake?") {
                (200, json!([]))
            } else if request.starts_with("GET /v1/e2ee/conversations/") {
                (
                    200,
                    json!({"e2ee":true,"epoch":has_group.then_some(0),"external_sender":b64(&key),"generation":0,"group_id":CONV,"head_hash":b64(&head),"repair_available":true}),
                )
            } else if request.contains("/group ") {
                (201, json!({"epoch":0}))
            } else if request.starts_with("POST /v1/conversations/") {
                (201, json!({"id":"accepted"}))
            } else {
                (200, json!({}))
            };
            super::super::super::tests::http(status, "Test", &body.to_string())
        });
        let saved = Saved {
            base_url: base,
            token: "t".into(),
            user_id: user.to_string(),
            email: None,
            display_name: "Ada".into(),
            role: "member".into(),
            plain_http_ok: false,
            e2ee: None,
        };
        let mut device = Device::new(&saved);
        device.dir = dir;
        device.engine = Some(engine);
        device.cache_key = Some(CacheKey::generate().unwrap());
        device.state = DeviceState::Ready;
        device.enrol();
        assert!(device.ready());
        device.verify_user(&user.to_string()).unwrap();
        (device, captured)
    }

    #[test]
    fn modern_info_and_handshake_requests_are_generation_fenced() {
        let (mut device, requests) = modern_device("read-fences", true);
        device.sync(CONV).unwrap();
        let requests = lock(&requests);
        let info = requests
            .iter()
            .find(|request| {
                request.starts_with(&format!("GET /v1/e2ee/conversations/{CONV}"))
                    && !request.contains("/handshake?")
            })
            .unwrap();
        assert!(
            info.lines().next().unwrap().contains("generation=0"),
            "modern info must declare its accepted generation: {info}"
        );
        let feed = requests
            .iter()
            .find(|request| request.contains("/handshake?"))
            .unwrap();
        assert!(
            feed.lines().next().unwrap().contains("generation=0"),
            "feed cursor must select a cryptographic generation"
        );
    }

    #[test]
    fn modern_outbound_message_carries_generation_without_leaking_group_secrets() {
        let (mut device, requests) = modern_device("message-fence", true);
        device
            .post(
                CONV,
                "one",
                Kind::Message,
                None,
                &Envelope {
                    text: "hello".into(),
                    ..Envelope::default()
                },
                &[],
            )
            .unwrap();
        let requests = lock(&requests);
        let posted = requests
            .iter()
            .find(|request| request.starts_with("POST /v1/conversations/"))
            .unwrap();
        let body: Value = serde_json::from_str(posted.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            body["generation"], 0,
            "a stale legacy-shaped send must never target the modern head implicitly"
        );
        assert!(body.get("ciphertext").and_then(Value::as_str).is_some());
        assert!(body.get("text").is_none());
    }

    #[test]
    fn modern_group_creation_declares_its_generation() {
        let (mut device, requests) = modern_device("create-fence", false);
        device.sync(CONV).unwrap();
        let requests = lock(&requests);
        let posted = requests
            .iter()
            .find(|request| request.contains("/group "))
            .unwrap();
        let body: Value = serde_json::from_str(posted.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["generation"], 0);
    }

    #[test]
    fn legacy_requests_keep_the_original_shape() {
        let (mut device, requests) = modern_device("legacy-parity", true);
        device.generations = false;
        lock(&requests).clear();
        device
            .post(
                CONV,
                "legacy",
                Kind::Message,
                None,
                &Envelope::default(),
                &[],
            )
            .unwrap();
        for request in lock(&requests).iter() {
            assert!(!request.lines().next().unwrap().contains("generation="));
            if let Some((_, body)) = request.split_once("\r\n\r\n") {
                if let Ok(body) = serde_json::from_str::<Value>(body) {
                    assert!(body.get("generation").is_none());
                }
            }
        }
    }

    #[test]
    fn prepared_or_accepted_generation_cannot_downgrade_when_capability_disappears() {
        for accepted in [false, true] {
            let (mut device, requests) = modern_device(
                if accepted {
                    "downgrade-accepted"
                } else {
                    "downgrade-prepared"
                },
                true,
            );
            let conversation = ConversationId::parse(CONV).unwrap();
            let binding = device
                .engine()
                .unwrap()
                .prepare_repair(conversation, 0, 0)
                .unwrap();
            if accepted {
                device
                    .engine()
                    .unwrap()
                    .mark_repair_submitted(conversation, binding.payload_hash().unwrap())
                    .unwrap();
                device.engine().unwrap().accept_repair(&binding).unwrap();
            }
            device.generations = false;
            lock(&requests).clear();
            assert!(device.sync(CONV).is_err());
            assert!(device
                .post(
                    CONV,
                    "blocked",
                    Kind::Message,
                    None,
                    &Envelope::default(),
                    &[]
                )
                .is_err());
            assert!(
                lock(&requests).is_empty(),
                "no legacy request may escape after durable modern state"
            );
        }
    }

    #[test]
    fn modern_info_without_generation_metadata_fails_before_feed_or_post() {
        let (mut device, _) = modern_device("missing-metadata", true);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let key = device
            .engine()
            .unwrap()
            .server_key(ConversationId::parse(CONV).unwrap())
            .unwrap();
        device.saved.base_url = super::super::super::tests::serve(move |request| {
            lock(&captured).push(request.to_owned());
            super::super::super::tests::http(
                200,
                "Test",
                &json!({"e2ee":true,"epoch":0,"external_sender":b64(&key)}).to_string(),
            )
        });
        assert!(device
            .post(
                CONV,
                "blocked",
                Kind::Message,
                None,
                &Envelope::default(),
                &[]
            )
            .is_err());
        let requests = lock(&requests);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET "));
    }

    #[test]
    fn native_cancel_never_prepares_or_posts_and_does_not_hold_the_device_lock() {
        struct Cancel(Shared);
        impl protocol::device_approval::DeviceApproval for Cancel {
            fn verify(
                &self,
                reason: &str,
                _: &dyn Fn() -> bool,
            ) -> Result<protocol::device_approval::Outcome, String> {
                assert_eq!(reason, "Approve updating encryption for this conversation");
                assert!(
                    self.0.try_lock().is_ok(),
                    "native UI must not hold DeviceLock"
                );
                Err("Canceled".into())
            }
        }
        let (device, requests) = modern_device("native-cancel", true);
        let saved = device.saved.clone();
        let shared = Arc::new(Mutex::new(device));
        lock(devices()).insert(
            (saved.base_url.clone(), saved.user_id.clone()),
            shared.clone(),
        );
        lock(&requests).clear();
        let job = Job {
            state: Mutex::new(protocol::E2eeRepair {
                conversation_id: CONV.into(),
                stage: protocol::RepairStage::Approving,
                error: String::new(),
            }),
            told: Arc::new(|| {}),
        };
        assert!(run(
            &job,
            &shared,
            &saved,
            CONV,
            &Cancel(shared.clone()),
            std::time::Instant::now() + std::time::Duration::from_secs(120)
        )
        .is_err());
        assert!(lock(&shared)
            .engine()
            .unwrap()
            .prepared_repair(ConversationId::parse(CONV).unwrap())
            .is_none());
        assert!(lock(&requests)
            .iter()
            .all(|request| !request.starts_with("POST ")));
        lock(devices()).remove(&(saved.base_url, saved.user_id));
    }

    #[test]
    fn repair_ipc_queues_without_waiting_for_device_lock_and_refuses_duplicate_jobs() {
        struct Cancel;
        impl protocol::device_approval::DeviceApproval for Cancel {
            fn verify(
                &self,
                _: &str,
                _: &dyn Fn() -> bool,
            ) -> Result<protocol::device_approval::Outcome, String> {
                Err("Canceled".into())
            }
        }
        let (device, _) = modern_device("queued-ipc", true);
        let saved = device.saved.clone();
        let shared = Arc::new(Mutex::new(device));
        lock(devices()).insert(
            (saved.base_url.clone(), saved.user_id.clone()),
            shared.clone(),
        );
        let held = lock(&shared);
        let started = std::time::Instant::now();
        super::start_with(&saved, CONV, Arc::new(|| {}), Arc::new(Cancel)).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(super::start_with(&saved, CONV, Arc::new(|| {}), Arc::new(Cancel)).is_err());
        assert!(
            super::acquire(&shared, std::time::Instant::now()).is_err(),
            "an expired job must not block waiting on DeviceLock"
        );
        drop(held);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while super::view(&saved).unwrap().stage != protocol::RepairStage::Failed {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        lock(devices()).remove(&(saved.base_url.clone(), saved.user_id.clone()));
        lock(super::jobs()).remove(&super::key(&saved));
    }

    #[test]
    fn unavailable_repair_does_not_prepare_a_candidate() {
        let (mut device, requests) = modern_device("unavailable-preflight", true);
        let server = remote(&mut device, requests.clone());
        lock(&server).available = false;
        lock(&requests).clear();
        assert_eq!(device.submit_repair(CONV).unwrap_err(), super::UNAVAILABLE);
        assert!(device
            .engine()
            .unwrap()
            .prepared_repair(ConversationId::parse(CONV).unwrap())
            .is_none());
        assert!(lock(&requests)
            .iter()
            .all(|request| !request.starts_with("POST ")));
    }

    #[test]
    fn uncertain_repair_posts_retry_the_exact_persisted_binding() {
        let (mut device, requests) = modern_device("uncertain-retry", true);
        lock(&requests).clear();
        assert!(
            device.submit_repair(CONV).is_err(),
            "the mock intentionally returns an unreadable repair response"
        );
        let conversation = ConversationId::parse(CONV).unwrap();
        let first = device
            .engine()
            .unwrap()
            .prepared_repair(conversation)
            .unwrap();
        assert!(first.submitted);
        assert!(device.submit_repair(CONV).is_err());
        assert_eq!(
            device
                .engine()
                .unwrap()
                .prepared_repair(conversation)
                .unwrap()
                .binding,
            first.binding
        );
        let posts: Vec<String> = lock(&requests)
            .iter()
            .filter(|request| request.contains("/repair "))
            .map(|request| request.split_once("\r\n\r\n").unwrap().1.to_owned())
            .collect();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0], posts[1]);
        assert!(device
            .engine()
            .unwrap()
            .discard_prepared_repair(conversation, first.binding.payload_hash().unwrap())
            .is_err());
    }

    #[test]
    fn a_new_generation_has_its_own_handshake_cursor() {
        let (mut device, requests) = modern_device("scoped-cursor", true);
        let conversation = ConversationId::parse(CONV).unwrap();
        let binding = device
            .engine()
            .unwrap()
            .prepare_repair(conversation, 0, 0)
            .unwrap();
        device
            .engine()
            .unwrap()
            .mark_repair_submitted(conversation, binding.payload_hash().unwrap())
            .unwrap();
        device.engine().unwrap().accept_repair(&binding).unwrap();
        device.notes.seqs.insert(CONV.into(), 999);
        let key = device
            .engine()
            .unwrap()
            .server_key(binding.group_id)
            .unwrap();
        lock(&requests).clear();
        device.feed(CONV, binding.group_id, &key).unwrap();
        let requests = lock(&requests);
        assert!(requests[0].lines().next().unwrap().contains("after=0&"));
        assert!(requests[0].lines().next().unwrap().contains("generation=1"));
    }

    #[test]
    fn a_cached_modern_message_refuses_stripped_or_forged_generation_metadata() {
        let (mut device, _) = modern_device("cached-generation", true);
        let conversation = ConversationId::parse(CONV).unwrap();
        let binding = device
            .engine()
            .unwrap()
            .prepare_repair(conversation, 0, 0)
            .unwrap();
        device
            .engine()
            .unwrap()
            .mark_repair_submitted(conversation, binding.payload_hash().unwrap())
            .unwrap();
        device.engine().unwrap().accept_repair(&binding).unwrap();
        let own = device.engine().unwrap().device_ref();
        let auth = Authentication::new(
            conversation,
            binding.group_id,
            1,
            0,
            &own,
            Kind::Message,
            None,
        );
        device.remember(
            CONV,
            "cached",
            Opened {
                text: "saved".into(),
                generation: Some(1),
                auth: Some(auth),
                ..Opened::default()
            },
        );
        let user = device.saved.user_id.clone();
        let own_id = own.device.to_string();
        let group = binding.group_id.to_string();
        for (generation, group_id, expected) in [
            (None, None, false),
            (Some(1), Some(CONV), false),
            (Some(1), Some(group.as_str()), true),
        ] {
            let row = Row {
                id: "cached",
                author_id: &user,
                kind: "message",
                ciphertext: None,
                epoch: 0,
                generation,
                group_id,
                device_id: &own_id,
                target_id: None,
                client_id: None,
                created_at: "",
            };
            let read = device.read(CONV, &[row]);
            assert_eq!(
                matches!(read.get("cached"), Some(Read::Opened(_))),
                expected
            );
        }
    }

    #[test]
    fn durable_group_state_never_falls_through_to_plaintext_discovery() {
        let (mut device, requests) = modern_device("no-plaintext-fallback", true);
        device.encrypted.insert(CONV.into(), false);
        lock(&requests).clear();
        assert!(device.is_encrypted(CONV).unwrap());
        let conversation = ConversationId::parse(CONV).unwrap();
        device
            .engine()
            .unwrap()
            .prepare_repair(conversation, 0, 0)
            .unwrap();
        device.generations = false;
        assert!(device.is_encrypted(CONV).is_err());
        assert!(lock(&requests).is_empty());
    }

    #[test]
    fn independent_sibling_metadata_decoder_does_not_coerce_invalid_generations_to_legacy() {
        for generation in [json!(-1), json!("1"), json!(null), json!(u64::MAX)] {
            assert!(
                super::row_metadata(&json!({"generation":generation,"group_id":CONV})).is_err()
            );
        }
        assert_eq!(super::row_metadata(&json!({})).unwrap(), (None, None));
        assert_eq!(
            super::row_metadata(&json!({"generation":1,"group_id":CONV})).unwrap(),
            (Some(1), Some(CONV))
        );
        assert!(super::row_metadata(&json!({"generation":1,"group_id":"bad"})).is_err());
    }

    #[test]
    fn accepted_repair_routes_new_sends_edits_reactions_and_keeps_old_cached_history() {
        let (mut device, requests) = modern_device("accepted-roundtrip", true);
        remote(&mut device, requests.clone());
        lock(&requests).clear();
        device
            .post(
                CONV,
                "old",
                Kind::Message,
                None,
                &Envelope::message("before"),
                &[],
            )
            .unwrap();
        device.submit_repair(CONV).unwrap();
        let active = device.active(CONV).unwrap();
        assert_eq!(active.generation, 1);
        assert_ne!(active.group_id.to_string(), CONV);
        device
            .post(
                CONV,
                "new",
                Kind::Message,
                None,
                &Envelope::message("after"),
                &[],
            )
            .unwrap();
        device
            .post(
                CONV,
                "edit",
                Kind::Edit,
                Some("message1"),
                &Envelope::message("edited"),
                &[],
            )
            .unwrap();
        device
            .react_generation(CONV, "message1", "👍", true)
            .unwrap();
        let group = active.group_id.to_string();
        let user = device.saved.user_id.clone();
        let own_id = device.engine().unwrap().device_id().to_string();
        let row = |id: &'static str, generation, group_id| Row {
            id,
            author_id: &user,
            kind: "message",
            ciphertext: None,
            epoch: 0,
            generation: Some(generation),
            group_id: Some(group_id),
            device_id: &own_id,
            target_id: None,
            client_id: None,
            created_at: "",
        };
        let edit = Row {
            id: "message3",
            author_id: &user,
            kind: "edit",
            ciphertext: None,
            epoch: 0,
            generation: Some(1),
            group_id: Some(&group),
            device_id: &own_id,
            target_id: Some("message1"),
            client_id: None,
            created_at: "",
        };
        let read = device.read(
            CONV,
            &[row("message1", 0, CONV), row("message2", 1, &group), edit],
        );
        assert!(
            matches!(read.get("message1"), Some(Read::Opened(opened)) if opened.text == "edited")
        );
        assert!(
            matches!(read.get("message2"), Some(Read::Opened(opened)) if opened.text == "after")
        );
        let requests = lock(&requests);
        let payloads: Vec<Value> = requests
            .iter()
            .filter(|request| {
                request.starts_with("POST /v1/conversations/") || request.contains("/reactions ")
            })
            .map(|request| serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap())
            .collect();
        assert_eq!(
            payloads
                .iter()
                .map(|body| body["generation"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [0, 1, 1, 1]
        );
        assert_eq!(payloads[2]["target_id"], "message1");
    }

    #[test]
    fn never_accepted_device_bootstraps_only_a_complete_pinned_chain_with_a_current_signer() {
        let (mut creator, requests) = modern_device("bootstrap-creator", true);
        let server = remote(&mut creator, requests.clone());
        creator.submit_repair(CONV).unwrap();
        let user = UserId::parse(&creator.saved.user_id).unwrap();
        let dir = std::env::temp_dir().join(format!("tiny-bootstrap-peer-{}", std::process::id()));
        private_dir(&dir).unwrap();
        let engine = Engine::create(
            user,
            Box::new(FileStore::new(&dir)),
            StateKey::generate().unwrap(),
            STATE,
        )
        .unwrap();
        let signature = creator
            .engine()
            .unwrap()
            .cross_sign(engine.device_id(), &engine.signature_key())
            .unwrap();
        lock(&server).directory.push(json!({"device_id":engine.device_id().to_string(),"signature_key":b64(&engine.signature_key()),"cross_signature":b64(&signature),"created_at":"2026-01-02T00:00:00Z"}));
        let mut peer = Device::new(&creator.saved);
        peer.dir = dir;
        peer.engine = Some(engine);
        peer.cache_key = Some(CacheKey::generate().unwrap());
        peer.state = DeviceState::Ready;
        peer.generations = true;
        // Binding fields and advertised heads must not supply trust or skip an invalid hash.
        lock(&server).corrupt_head = true;
        assert!(peer.discover_generations(CONV).is_err());
        assert!(peer.active(CONV).unwrap().head_hash.is_none());
        lock(&server).corrupt_head = false;
        peer.discover_generations(CONV).unwrap();
        assert_eq!(peer.active(CONV).unwrap(), creator.active(CONV).unwrap());
        let internal = peer.active(CONV).unwrap().group_id;
        assert!(
            !peer.engine().unwrap().has_group(internal),
            "bootstrap still requires ordinary MLS admission"
        );
        // Current directory revocation makes the very same signed binding unacceptable to
        // a new never-accepted device; no trust comes from the binding's embedded key.
        lock(&server).directory.clear();
        let (mut refused, _) = modern_device("bootstrap-revoked-signer", false);
        refused.saved.base_url = creator.saved.base_url.clone();
        assert!(refused.discover_generations(CONV).is_err());
        assert!(refused.active(CONV).unwrap().head_hash.is_none());
    }

    #[test]
    fn cached_plaintext_id_cannot_be_relabelled_as_another_author_or_device() {
        let (mut device, _) = modern_device("cached-author-forgery", true);
        device
            .post(
                CONV,
                "real-ada",
                Kind::Message,
                None,
                &Envelope::message("Ada wrote this"),
                &[],
            )
            .unwrap();
        let row = Row {
            id: "accepted",
            author_id: "33333333-3333-3333-3333-333333333333",
            kind: "message",
            ciphertext: None,
            epoch: 0,
            generation: Some(0),
            group_id: Some(CONV),
            device_id: "44444444-4444-4444-4444-444444444444",
            target_id: None,
            client_id: None,
            created_at: "",
        };
        let read = device.read(CONV, &[row]);
        assert!(
            !matches!(read.get("accepted"), Some(Read::Opened(_))),
            "a cache hit must not present Ada's plaintext with Bea's wire identity"
        );
    }

    #[test]
    fn own_pending_echo_cannot_be_relabelled_as_another_author() {
        let (mut device, requests) = modern_device("pending-author-forgery", true);
        let server = remote(&mut device, requests);
        lock(&server).refuse_posts = true;
        assert!(device
            .post(
                CONV,
                "pending-ada",
                Kind::Message,
                None,
                &Envelope::message("Ada's pending text"),
                &[]
            )
            .is_err());
        let row = Row {
            id: "forged",
            author_id: "33333333-3333-3333-3333-333333333333",
            kind: "message",
            ciphertext: None,
            epoch: 0,
            generation: Some(0),
            group_id: Some(CONV),
            device_id: "44444444-4444-4444-4444-444444444444",
            target_id: None,
            client_id: Some("pending-ada"),
            created_at: "",
        };
        let read = device.read(CONV, &[row]);
        assert!(
            !matches!(read.get("forged"), Some(Read::Opened(_))),
            "a server echo must match the locally authenticated sending identity"
        );
    }

    #[test]
    fn imported_modern_cache_validates_without_old_private_groups_and_survives_capability_loss() {
        let (mut device, _) = modern_device("imported-auth-cache", false);
        let own = device.engine().unwrap().device_ref();
        let conversation = ConversationId::parse(CONV).unwrap();
        let internal = ConversationId::parse("88888888-8888-8888-8888-888888888888").unwrap();
        device.remember(
            CONV,
            "imported",
            Opened {
                text: "history from another device".into(),
                generation: Some(3),
                auth: Some(Authentication::new(
                    conversation,
                    internal,
                    3,
                    5,
                    &own,
                    Kind::Message,
                    None,
                )),
                imported_from: Some("approved-old-device".into()),
                ..Opened::default()
            },
        );
        assert!(device.engine().unwrap().group_at(conversation, 3).is_err());
        device.generations = false;
        let author = own.user.to_string();
        let from = own.device.to_string();
        let group = internal.to_string();
        let row = Row {
            id: "imported",
            author_id: &author,
            device_id: &from,
            generation: Some(3),
            group_id: Some(&group),
            epoch: 5,
            kind: "message",
            target_id: None,
            ciphertext: None,
            client_id: None,
            created_at: "",
        };
        assert!(
            matches!(device.read(CONV, &[row]).get("imported"), Some(Read::Opened(opened)) if opened.text == "history from another device")
        );
    }

    #[test]
    fn legacy_plaintext_stays_readable_without_accepting_a_server_author_or_authorizing_an_edit() {
        let (mut device, _) = modern_device("legacy-neutral-cache", true);
        device.remember(
            CONV,
            "legacy",
            Opened {
                text: "saved before this update".into(),
                ..Opened::default()
            },
        );
        let user = device.saved.user_id.clone();
        let own = device.engine().unwrap().device_ref();
        let author = own.user.to_string();
        let from = own.device.to_string();
        let edit = Opened {
            text: "must not replace legacy".into(),
            edits: Some("legacy".into()),
            auth: Some(Authentication::new(
                ConversationId::parse(CONV).unwrap(),
                ConversationId::parse(CONV).unwrap(),
                0,
                0,
                &own,
                Kind::Edit,
                Some("legacy"),
            )),
            ..Opened::default()
        };
        device.remember(CONV, "edit", edit);
        let rows = [
            Row {
                id: "legacy",
                author_id: &user,
                device_id: "forged",
                generation: None,
                group_id: None,
                epoch: 0,
                kind: "message",
                target_id: None,
                ciphertext: None,
                client_id: None,
                created_at: "",
            },
            Row {
                id: "edit",
                author_id: &author,
                device_id: &from,
                generation: Some(0),
                group_id: Some(CONV),
                epoch: 0,
                kind: "edit",
                target_id: Some("legacy"),
                ciphertext: None,
                client_id: None,
                created_at: "",
            },
        ];
        let read = device.read(CONV, &rows);
        assert!(
            matches!(read.get("legacy"), Some(Read::Legacy(opened)) if opened.text == "saved before this update" && opened.auth.is_none())
        );
        assert_eq!(
            device.cache(CONV).unwrap().rows["legacy"].text,
            "saved before this update"
        );
    }

    #[test]
    fn an_authenticated_edit_cannot_replace_another_authors_cached_target_even_when_target_is_not_in_page(
    ) {
        let (mut device, _) = modern_device("cross-author-cached-edit", true);
        let conversation = ConversationId::parse(CONV).unwrap();
        let ada = device.engine().unwrap().device_ref();
        let bea = Engine::detached(UserId::parse("33333333-3333-3333-3333-333333333333").unwrap())
            .unwrap()
            .device_ref();
        device.remember(
            CONV,
            "target",
            Opened {
                text: "Ada original".into(),
                auth: Some(Authentication::new(
                    conversation,
                    conversation,
                    0,
                    0,
                    &ada,
                    Kind::Message,
                    None,
                )),
                ..Opened::default()
            },
        );
        device.remember(
            CONV,
            "edit",
            Opened {
                text: "Bea replacement".into(),
                edits: Some("target".into()),
                auth: Some(Authentication::new(
                    conversation,
                    conversation,
                    0,
                    0,
                    &bea,
                    Kind::Edit,
                    Some("target"),
                )),
                ..Opened::default()
            },
        );
        let author = bea.user.to_string();
        let from = bea.device.to_string();
        let row = Row {
            id: "edit",
            author_id: &author,
            device_id: &from,
            generation: Some(0),
            group_id: Some(CONV),
            epoch: 0,
            kind: "edit",
            target_id: Some("target"),
            ciphertext: None,
            client_id: None,
            created_at: "",
        };
        let read = device.read(CONV, &[row]);
        assert!(matches!(read.get("edit"), Some(Read::Edit)));
        assert!(!read.contains_key("target"));
        assert_eq!(
            device.cache(CONV).unwrap().rows["target"].text,
            "Ada original"
        );
    }
    #[test]
    fn pending_bootstrap_reopens_and_recovers_only_through_read_only_exact_chain_retry() {
        use e2ee::store::{MemoryStore, Store};
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Clone, Default)]
        struct Failing {
            inner: Arc<MemoryStore>,
            until_failure: Arc<AtomicUsize>,
        }
        impl Store for Failing {
            fn load(&self, name: &str) -> e2ee::Result<Option<Vec<u8>>> {
                self.inner.load(name)
            }
            fn remove(&self, name: &str) -> e2ee::Result<()> {
                self.inner.remove(name)
            }
            fn save(&self, name: &str, bytes: &[u8]) -> e2ee::Result<()> {
                let mut remaining = self.until_failure.load(Ordering::SeqCst);
                while remaining > 0 {
                    match self.until_failure.compare_exchange(
                        remaining,
                        remaining - 1,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(_) if remaining == 1 => {
                            return Err(Error::Storage("injected activation write failure".into()))
                        }
                        Ok(_) => break,
                        Err(actual) => remaining = actual,
                    }
                }
                self.inner.save(name, bytes)
            }
        }
        let (mut creator, requests) = modern_device("retry-bootstrap-creator", true);
        let server = remote(&mut creator, requests.clone());
        creator.submit_repair(CONV).unwrap();
        let mut peer = Device::new(&creator.saved);
        peer.dir =
            std::env::temp_dir().join(format!("tiny-pending-bootstrap-{}", std::process::id()));
        private_dir(&peer.dir).unwrap();
        let state_key = StateKey::generate().unwrap();
        let store = Failing::default();
        let engine = Engine::create(
            UserId::parse(&creator.saved.user_id).unwrap(),
            Box::new(store.clone()),
            state_key.clone(),
            STATE,
        )
        .unwrap();
        let signature = creator
            .engine()
            .unwrap()
            .cross_sign(engine.device_id(), &engine.signature_key())
            .unwrap();
        lock(&server).directory.push(json!({"device_id":engine.device_id().to_string(),"signature_key":b64(&engine.signature_key()),"cross_signature":b64(&signature),"created_at":"2026-01-02T00:00:00Z"}));
        peer.engine = Some(engine);
        peer.cache_key = Some(CacheKey::generate().unwrap());
        peer.generations = true;
        peer.state = DeviceState::Ready;
        // Fresh directory persistence, successor observation, then failed activation.
        store.until_failure.store(3, Ordering::SeqCst);
        assert!(peer.discover_generations(CONV).is_err());
        peer.engine = Engine::open(Box::new(store.clone()), state_key, STATE).unwrap();
        assert!(matches!(
            peer.engine()
                .unwrap()
                .active_group(ConversationId::parse(CONV).unwrap()),
            Err(Error::RepairPending)
        ));
        lock(&requests).clear();
        let (active, _) = peer.group_info(CONV).unwrap();
        assert_eq!(active, creator.active(CONV).unwrap());
        let requests = lock(&requests);
        assert!(requests
            .iter()
            .any(|request| request.contains("/generations?after=0&limit=100")));
        assert!(
            requests.iter().all(|request| request.starts_with("GET ")),
            "no legacy create/send or duplicate repair POST may escape while recovering bootstrap"
        );
    }
    #[test]
    fn review_registry_lookup_does_not_hold_global_lock_while_waiting_for_a_device() {
        let (device, _) = modern_device("registry-lock-order", true);
        let saved = device.saved.clone();
        let shared = Arc::new(Mutex::new(device));
        lock(devices()).insert(
            (saved.base_url.clone(), saved.user_id.clone()),
            shared.clone(),
        );
        let held = lock(&shared);
        let (entered, waiting) = std::sync::mpsc::channel();
        let lookup_saved = saved.clone();
        let lookup = std::thread::spawn(move || {
            entered.send(()).unwrap();
            super::super::device(&lookup_saved)
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        let settle = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while devices().try_lock().is_ok() && std::time::Instant::now() < settle {
            std::thread::yield_now();
        }
        let (probed, probe) = std::sync::mpsc::channel();
        let probe_saved = saved.clone();
        let probe_thread = std::thread::spawn(move || {
            probed
                .send(super::super::started(&probe_saved).is_some())
                .unwrap();
        });
        let registry_remains_available = probe
            .recv_timeout(std::time::Duration::from_millis(250))
            .is_ok();
        // Break the old circular wait before asserting, so RED is bounded and leaves no hung threads.
        drop(held);
        lookup.join().unwrap();
        probe_thread.join().unwrap();
        lock(devices()).remove(&(saved.base_url, saved.user_id));
        assert!(registry_remains_available, "Status token refresh must not lock registry then Device while repair locks Device then registry");
    }

    #[test]
    fn review_new_generation_two_bootstrap_checks_only_current_terminal_signer_but_existing_peer_refuses_revoked_history(
    ) {
        let (mut creator, requests) = modern_device("gen2-terminal-creator", true);
        let server = remote(&mut creator, requests.clone());
        creator.submit_repair(CONV).unwrap();
        let first = lock(&server).bindings[0].clone();
        let signer = e2ee::identity::DeviceKey::generate().unwrap();
        let signer_id = DeviceId::parse("77777777-7777-4777-8777-777777777777").unwrap();
        let mut second = first.clone();
        second.from_generation = 1;
        second.to_generation = 2;
        second.from_group_id = first.group_id;
        second.group_id = ConversationId::parse("88888888-8888-4888-8888-888888888888").unwrap();
        second.previous_hash = first.payload_hash().unwrap();
        second.from_epoch = 0;
        second.device_id = signer_id;
        second.signature_key = signer.public_key();
        second.signature = signer.sign(&second.canonical_payload().unwrap());
        second.verify_signature().unwrap();
        let mut peer = Device::new(&creator.saved);
        peer.dir = std::env::temp_dir().join(format!("tiny-gen2-bootstrap-{}", std::process::id()));
        private_dir(&peer.dir).unwrap();
        let engine = Engine::create(
            UserId::parse(&creator.saved.user_id).unwrap(),
            Box::new(FileStore::new(&peer.dir)),
            StateKey::generate().unwrap(),
            STATE,
        )
        .unwrap();
        let own_signature = signer.sign(&e2ee::identity::cross_signature_message(
            engine.user_id(),
            engine.device_id(),
            &engine.signature_key(),
        ));
        {
            let mut state = lock(&server);
            state.bindings.push(second.clone());
            state.directory = vec![
                json!({"device_id":signer_id.to_string(),"signature_key":b64(&signer.public_key()),"created_at":"2026-01-02T00:00:00Z"}),
                json!({"device_id":engine.device_id().to_string(),"signature_key":b64(&engine.signature_key()),"cross_signature":b64(&own_signature),"created_at":"2026-01-03T00:00:00Z"}),
            ];
        }
        peer.engine = Some(engine);
        peer.state = DeviceState::Ready;
        peer.generations = true;
        peer.cache_key = Some(CacheKey::generate().unwrap());
        let (mut known, _) = modern_device("gen2-existing-old-peer", false);
        let pinned_key = lock(&server).key.clone();
        known
            .engine()
            .unwrap()
            .create_group(ConversationId::parse(CONV).unwrap(), &pinned_key)
            .unwrap();
        known.saved.base_url = creator.saved.base_url.clone();
        lock(&requests).clear();
        assert!(
            known.discover_generations(CONV).is_err(),
            "existing state must not bypass the revoked predecessor signer/old roster"
        );
        assert_eq!(known.active(CONV).unwrap().generation, 0);
        assert!(
            lock(&requests)
                .iter()
                .any(|request| request.starts_with("GET /v1/e2ee/users/")),
            "existing peer must independently check the historical signer's current directory"
        );
        peer.discover_generations(CONV).expect("never-accepted group bootstrap must not require a revoked historical signer to be current");
        assert_eq!(peer.active(CONV).unwrap().generation, 2);
        assert_eq!(peer.active(CONV).unwrap().group_id, second.group_id);
    }
}
