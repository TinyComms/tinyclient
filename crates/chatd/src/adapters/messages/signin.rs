//! Signing in to Android Messages. The phone scans a code from Messages' device pairing screen.
//! This computer never becomes the phone: there is no number to type.

use super::bugle;
use super::fake::FakeLinker;
use super::scripted;
use crate::hub::Hub;
use protocol::{AccountKey, Change, Network, SignInInput, SignInProblem, SignInStep};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub type Adopt = Box<dyn Fn(&str) -> Result<Vec<AccountKey>, String> + Send + Sync>;

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

pub fn step(hub: &Arc<Hub>, input: Option<SignInInput>, adopt: Adopt) -> Result<SignInStep, String> {
    let mut held = current();
    let live = held.as_ref().filter(|flow| !flow.ended.load(Ordering::Relaxed)).cloned();
    let flow = match (live, &input) {
        (Some(flow), _) => flow,
        (None, Some(_)) => return Ok(SignInStep::Failed { reason: "That sign-in is over. Start again.".to_owned() }),
        (None, None) => {
            let flow = Arc::new(Flow { hub: hub.clone(), step: Mutex::new(waiting()), ended: AtomicBool::new(false), cancelled: Arc::default() });
            let running = flow.clone();
            std::thread::Builder::new().name("messages-link".into()).spawn(move || running.run(adopt)).map_err(|err| err.to_string())?;
            *held = Some(flow.clone());
            flow
        }
    };
    drop(held);
    let step = flow.step().clone();
    Ok(match input {
        Some(SignInInput::UsePhone | SignInInput::Phone { .. } | SignInInput::Code { .. } | SignInInput::Password { .. }) => match step {
            SignInStep::Qr { link, expires, .. } => SignInStep::Qr {
                link,
                expires,
                problem: Some(SignInProblem::Other {
                    reason: "Messages pairs a computer from your phone: open Messages there and scan this code.".to_owned(),
                }),
            },
            other => other,
        },
        _ => step,
    })
}

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
            self.hub.changed(Change::SignIn { network: Network::Messages, step });
        }
    }

    fn run(&self, adopt: Adopt) {
        let mut tries = 0;
        let outcome = loop {
            let show = |link: String| self.show(SignInStep::Qr { link, expires: 0, problem: None });
            let linked = if scripted() {
                FakeLinker::link(&show, &self.cancelled)
            } else {
                bugle::pair(&show, &self.cancelled).and_then(|saved| serde_json::to_string(&saved).map_err(|err| err.to_string()))
            };
            match linked {
                Ok(session) => break Ok(session),
                Err(_) if self.cancelled.load(Ordering::Relaxed) => return,
                Err(why) if why == "expired" || why.contains("expired") => {
                    tries += 1;
                    if tries >= TRIES {
                        break Err("The code kept expiring. Check that the phone is online and try again.".to_owned());
                    }
                }
                Err(why) => break Err(why),
            }
        };
        if self.cancelled.load(Ordering::Relaxed) {
            return;
        }
        let step = match outcome {
            Ok(session) => match adopt(&session) {
                Ok(accounts) => SignInStep::Done { account: accounts.into_iter().next() },
                Err(why) => SignInStep::Failed { reason: format!("Messages paired, but tinyclient could not keep the session: {why}") },
            },
            Err(reason) => SignInStep::Failed { reason },
        };
        self.show(step);
        self.ended.store(true, Ordering::Relaxed);
    }
}
