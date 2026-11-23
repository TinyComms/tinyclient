//! Signing in to Signal, as the shell's sheet draws it: this device is linked to the account as
//! one more companion device, from your primary phone (Settings > Linked devices > Link new
//! device), by scanning a code or opening the `sgnl://linkdevice` link URI. There is no number to
//! type: registering a number here would make this device the account's primary phone and sign the
//! real phone out, so it is never offered.
//!
//! After the phone approves the link, if the primary phone supplied an ephemeral backup key for
//! Signal's linked-device message history transfer, the flow transitions through
//! `SignInStep::Transfer` (waiting -> downloading -> importing, or declined / expired / failed)
//! while keeping a clear "Skip" action at every point. The device stays linked regardless of
//! whether history transfer completes, is declined, expires, fails, or is skipped.
//!
//! One sign-in at a time. It lives from `SignInBegin` to the step that ends it or to
//! `SignInCancel`, on a thread of its own; nothing of Signal's is loaded before it begins. A link
//! the user cancels before the phone finishes linking is undone on the account (the linker's duty).

use super::wire::{Fault, HistoryTransfer, TransferOutcome, TransferProgress};
use super::{linker, parse, supported, transfer, unsupported_reason};
use crate::hub::Hub;
use protocol::{AccountKey, Change, Network, SignInInput, SignInProblem, SignInStep, TransferStage};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Keeps a finished session and starts its accounts.
pub type Adopt = Box<dyn Fn(&str) -> Result<Vec<AccountKey>, String> + Send + Sync>;

/// What this computer is called in the phone's list of linked devices.
const DEVICE: &str = "tinyclient";
/// A link that could not reach Signal is tried this many times before the sheet says so.
const TRIES: u32 = 3;

struct Flow {
    hub: Arc<Hub>,
    step: Mutex<SignInStep>,
    ended: AtomicBool,
    cancelled: Arc<AtomicBool>,
    skip_transfer: Arc<AtomicBool>,
    wake: (Mutex<bool>, Condvar),
}

static FLOW: Mutex<Option<Arc<Flow>>> = Mutex::new(None);

fn current() -> MutexGuard<'static, Option<Arc<Flow>>> {
    FLOW.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn waiting() -> SignInStep {
    SignInStep::Qr { link: String::new(), expires: 0, problem: None }
}

fn transfer_step(
    stage: TransferStage,
    chats: u32,
    messages: u32,
    downloaded_bytes: u64,
    total_bytes: u64,
    detail: impl Into<String>,
) -> SignInStep {
    SignInStep::Transfer {
        stage,
        chats,
        messages,
        downloaded_bytes,
        total_bytes,
        detail: detail.into(),
    }
}

/// Begins a sign-in, or with `input` answers the step on show. Answers the step to draw.
/// `Err` when this build cannot sign in to Signal at all, and why.
pub fn step(hub: &Arc<Hub>, input: Option<SignInInput>, adopt: Adopt) -> Result<SignInStep, String> {
    if !supported() {
        return Err(unsupported_reason());
    }
    let mut held = current();
    let live = held.as_ref().filter(|flow| !flow.ended.load(Ordering::Relaxed)).cloned();
    let flow = match (live, &input) {
        (Some(flow), _) => flow,
        (None, Some(_)) => return Ok(SignInStep::Failed { reason: "That sign-in is over. Start again.".to_owned() }),
        (None, None) => {
            let linker = linker()?;
            let flow = Arc::new(Flow {
                hub: hub.clone(),
                step: Mutex::new(waiting()),
                ended: AtomicBool::new(false),
                cancelled: Arc::default(),
                skip_transfer: Arc::default(),
                wake: (Mutex::new(false), Condvar::new()),
            });
            let running = flow.clone();
            super::presage_thread("signal-link")
                .spawn(move || running.run(linker, adopt))
                .map_err(|err| err.to_string())?;
            *held = Some(flow.clone());
            flow
        }
    };
    drop(held);
    if matches!(input, Some(SignInInput::Skip)) {
        flow.request_skip();
    }
    let step = flow.step().clone();
    Ok(match input {
        // Every other way in is a phone number's, which Signal does not offer a second device.
        Some(SignInInput::UsePhone | SignInInput::Phone { .. } | SignInInput::Code { .. } | SignInInput::Password { .. }) => match step {
            SignInStep::Qr { link, expires, .. } => SignInStep::Qr {
                link,
                expires,
                problem: Some(SignInProblem::Other {
                    reason: "Signal links this device from your primary phone: open Signal there and scan this code.".to_owned(),
                }),
            },
            other => other,
        },
        _ => step,
    })
}

/// The user closed the sheet.
pub fn cancel() {
    if let Some(flow) = current().take() {
        flow.cancelled.store(true, Ordering::Relaxed);
        flow.request_skip();
        flow.ended.store(true, Ordering::Relaxed);
    }
}

impl Flow {
    fn step(&self) -> MutexGuard<'_, SignInStep> {
        self.step.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn request_skip(&self) {
        self.skip_transfer.store(true, Ordering::Relaxed);
        let (lock, cvar) = &self.wake;
        *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
        cvar.notify_all();
    }

    fn wait_for_ack(&self) {
        let (lock, cvar) = &self.wake;
        let mut guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        while !*guard && !self.skip_transfer.load(Ordering::Relaxed) && !self.cancelled.load(Ordering::Relaxed) {
            guard = cvar.wait(guard).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Shows a step, and tells the shell when it changed without being asked.
    fn show(&self, step: SignInStep) {
        if self.cancelled.load(Ordering::Relaxed) {
            return;
        }
        let changed = {
            let mut held = self.step();
            let changed = *held != step;
            *held = step.clone();
            changed
        };
        if changed {
            self.hub.changed(Change::SignIn { network: Network::Signal, step });
        }
    }

    fn run(&self, mut linker: Box<dyn super::wire::Linker>, adopt: Adopt) {
        let mut tries = 0;
        let outcome = loop {
            let show = |link: String| self.show(SignInStep::Qr { link, expires: 0, problem: None });
            match linker.link(DEVICE, &show, self.cancelled.clone()) {
                Ok(linked) => break Ok(linked),
                Err(_) if self.cancelled.load(Ordering::Relaxed) => return,
                // The code ran out or the network went away before the phone scanned it: a new
                // code, a few times, before the sheet says so.
                Err(Fault::Net(why)) if tries + 1 < TRIES => {
                    tries += 1;
                    tracing::warn!("chatd: Signal: linking: {}", super::super::net::redact(&why));
                    self.show(SignInStep::Qr { link: String::new(), expires: 0, problem: Some(SignInProblem::Unreachable) });
                    linker = match super::linker() {
                        Ok(linker) => linker,
                        Err(why) => break Err(why),
                    };
                }
                Err(fault) => break Err(fault.words_for_link()),
            }
        };
        let (account, saved, transfer_handle) = match outcome {
            Ok(linked) => match adopt(&linked.session) {
                Ok(accounts) => {
                    let account = accounts.into_iter().next();
                    let saved = parse(&linked.session).ok();
                    (account, saved, linked.transfer)
                }
                Err(why) => {
                    self.show(SignInStep::Failed {
                        reason: format!("Signal linked, but tinyclient could not keep the session: {why}"),
                    });
                    self.ended.store(true, Ordering::Relaxed);
                    return;
                }
            },
            Err(reason) => {
                self.show(SignInStep::Failed { reason });
                self.ended.store(true, Ordering::Relaxed);
                return;
            }
        };

        if let (Some(account_key), Some(saved), Some(transfer_handle)) = (&account, &saved, transfer_handle) {
            self.run_transfer(account_key, saved, transfer_handle);
        }

        self.show(SignInStep::Done { account });
        self.ended.store(true, Ordering::Relaxed);
    }

    fn run_transfer(&self, account: &AccountKey, saved: &super::Saved, transfer_handle: Box<dyn HistoryTransfer>) {
        if self.skip_transfer.load(Ordering::Relaxed) || self.cancelled.load(Ordering::Relaxed) {
            return;
        }
        self.show(transfer_step(TransferStage::Waiting, 0, 0, 0, 0, ""));

        let on_download_progress = |progress: TransferProgress| match progress {
            TransferProgress::Waiting => {
                self.show(transfer_step(TransferStage::Waiting, 0, 0, 0, 0, ""));
            }
            TransferProgress::Downloading { downloaded_bytes, total_bytes } => {
                self.show(transfer_step(
                    TransferStage::Downloading,
                    0,
                    0,
                    downloaded_bytes,
                    total_bytes,
                    "",
                ));
            }
        };

        match transfer_handle.fetch(&on_download_progress, self.skip_transfer.clone()) {
            TransferOutcome::Ready { path, ephemeral_key, total_bytes } => {
                if self.skip_transfer.load(Ordering::Relaxed) || self.cancelled.load(Ordering::Relaxed) {
                    let _ = std::fs::remove_file(&path);
                    return;
                }
                self.show(transfer_step(TransferStage::Importing, 0, 0, total_bytes, total_bytes, ""));
                let on_import_progress = |chats: u32, messages: u32| {
                    self.show(transfer_step(
                        TransferStage::Importing,
                        chats,
                        messages,
                        total_bytes,
                        total_bytes,
                        "",
                    ));
                };
                let result = transfer::import_archive(
                    &self.hub,
                    account,
                    &saved.aci,
                    &saved.name,
                    &ephemeral_key,
                    &path,
                    &on_import_progress,
                    &self.skip_transfer,
                );
                // Always remove the temporary encrypted transfer archive once import finishes or fails.
                let _ = std::fs::remove_file(&path);
                if let Err(why) = result {
                    if !self.skip_transfer.load(Ordering::Relaxed) && !self.cancelled.load(Ordering::Relaxed) {
                        tracing::warn!("chatd: Signal: history transfer import failed: {why}");
                        self.show(transfer_step(TransferStage::Failed, 0, 0, total_bytes, total_bytes, why));
                        self.wait_for_ack();
                    }
                }
            }
            TransferOutcome::Declined(why) => {
                if !self.skip_transfer.load(Ordering::Relaxed) && !self.cancelled.load(Ordering::Relaxed) {
                    self.show(transfer_step(TransferStage::Declined, 0, 0, 0, 0, why));
                    self.wait_for_ack();
                }
            }
            TransferOutcome::Expired(why) => {
                if !self.skip_transfer.load(Ordering::Relaxed) && !self.cancelled.load(Ordering::Relaxed) {
                    self.show(transfer_step(TransferStage::Expired, 0, 0, 0, 0, why));
                    self.wait_for_ack();
                }
            }
            TransferOutcome::Failed(why) => {
                if !self.skip_transfer.load(Ordering::Relaxed) && !self.cancelled.load(Ordering::Relaxed) {
                    self.show(transfer_step(TransferStage::Failed, 0, 0, 0, 0, why));
                    self.wait_for_ack();
                }
            }
            TransferOutcome::Skipped => {}
        }
    }
}

impl Fault {
    fn words_for_link(&self) -> String {
        match self {
            Fault::Net(_) => "Couldn't reach Signal. Check your connection and try again.".to_owned(),
            Fault::Refused(why) | Fault::Ended(why) => why.clone(),
        }
    }
}
