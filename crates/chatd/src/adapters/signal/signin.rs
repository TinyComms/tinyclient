//! Signing in to Signal, as the shell's sheet draws it: this computer is linked to the account as
//! one more device, from the phone (Settings > Linked devices > Link new device), by scanning a
//! code. There is no number to type: registering a number here would make this computer the
//! account's phone and sign the real phone out, so it is never offered.
//!
//! One sign-in at a time. It lives from `SignInBegin` to the step that ends it or to
//! `SignInCancel`, on a thread of its own; nothing of Signal's is loaded before it begins. A link
//! the user cancels after the phone accepted it is undone on the account (the linker's duty).

use super::wire::Fault;
use super::{linker, supported, unsupported_reason};
use crate::hub::Hub;
use protocol::{AccountKey, Change, Network, SignInInput, SignInProblem, SignInStep};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

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
}

static FLOW: Mutex<Option<Arc<Flow>>> = Mutex::new(None);

fn current() -> MutexGuard<'static, Option<Arc<Flow>>> {
    FLOW.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn waiting() -> SignInStep {
    SignInStep::Qr { link: String::new(), expires: 0, problem: None }
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
            let flow = Arc::new(Flow { hub: hub.clone(), step: Mutex::new(waiting()), ended: AtomicBool::new(false), cancelled: Arc::default() });
            let running = flow.clone();
            std::thread::Builder::new()
                .name("signal-link".into())
                .spawn(move || running.run(linker, adopt))
                .map_err(|err| err.to_string())?;
            *held = Some(flow.clone());
            flow
        }
    };
    drop(held);
    let step = flow.step().clone();
    Ok(match input {
        // Every other way in is a phone number's, which Signal does not offer a second device.
        Some(SignInInput::UsePhone | SignInInput::Phone { .. } | SignInInput::Code { .. } | SignInInput::Password { .. }) => match step {
            SignInStep::Qr { link, expires, .. } => SignInStep::Qr {
                link,
                expires,
                problem: Some(SignInProblem::Other {
                    reason: "Signal links a computer from your phone: open Signal there and scan this code.".to_owned(),
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
        flow.ended.store(true, Ordering::Relaxed);
    }
}

impl Flow {
    fn step(&self) -> MutexGuard<'_, SignInStep> {
        self.step.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
                    eprintln!("chatd: Signal: linking: {}", super::super::net::redact(&why));
                    self.show(SignInStep::Qr { link: String::new(), expires: 0, problem: Some(SignInProblem::Unreachable) });
                    linker = match super::linker() {
                        Ok(linker) => linker,
                        Err(why) => break Err(why),
                    };
                }
                Err(fault) => break Err(fault.words_for_link()),
            }
        };
        let step = match outcome {
            Ok(linked) => match adopt(&linked.session) {
                Ok(accounts) => SignInStep::Done { account: accounts.into_iter().next() },
                Err(why) => SignInStep::Failed { reason: format!("Signal linked, but tinyclient could not keep the session: {why}") },
            },
            Err(reason) => SignInStep::Failed { reason },
        };
        self.show(step);
        self.ended.store(true, Ordering::Relaxed);
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
