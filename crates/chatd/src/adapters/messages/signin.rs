//! Google Messages sign-in: either QR-code pairing (no Google account or browser helper needed)
//! or Google account sign-in in a browser followed by an emoji confirmation on the phone.

use super::{bugle, fake::FakeLinker, scripted};
use crate::hub::Hub;
use protocol::{AccountKey, Change, Network, SignInInput, SignInProblem, SignInStep};
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub type Adopt = Box<dyn Fn(&str) -> Result<Vec<AccountKey>, String> + Send + Sync>;

/// How many QR codes are offered before giving up on repeated expiry.
const QR_TRIES: u32 = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Choice,
    Google,
    Qr,
}

struct Flow {
    hub: Arc<Hub>,
    step: Mutex<SignInStep>,
    mode: Mutex<Mode>,
    running: AtomicBool,
    ended: AtomicBool,
    cancelled: AtomicBool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_worker_cannot_adopt_or_publish_into_a_restarted_flow() {
        let hub = Arc::new(Hub::new(crate::store::Store::open(std::path::Path::new(":memory:")).unwrap()));
        step(&hub, None, Box::new(|_| Ok(Vec::new()))).unwrap();
        let old = current().as_ref().unwrap().clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let adopted = Arc::new(AtomicBool::new(false));
        let worker = {
            let (old, barrier, adopted) = (old.clone(), barrier.clone(), adopted.clone());
            std::thread::spawn(move || {
                barrier.wait();
                old.show(SignInStep::Emoji { emoji: "late".into() });
                old.finish(Ok("late-session".into()), Box::new(move |_| {
                    adopted.store(true, Ordering::Release);
                    Ok(Vec::new())
                }));
            })
        };
        cancel();
        step(&hub, None, Box::new(|_| Ok(Vec::new()))).unwrap();
        let seq = hub.seq();
        barrier.wait();
        worker.join().unwrap();
        assert!(!adopted.load(Ordering::Acquire));
        assert_eq!(hub.seq(), seq, "cancelled worker published a stale event");
        assert_eq!(*old.step(), browser(false));
        assert_eq!(step(&hub, None, Box::new(|_| Ok(Vec::new()))).unwrap(), browser(false));
        cancel();
    }

    #[test]
    fn switching_between_qr_and_browser_choice_cancels_the_previous_flow() {
        let hub = Arc::new(Hub::new(crate::store::Store::open(std::path::Path::new(":memory:")).unwrap()));
        assert_eq!(step(&hub, None, Box::new(|_| Ok(Vec::new()))).unwrap(), browser(false));
        let first = current().as_ref().unwrap().clone();
        let qr = step(&hub, Some(SignInInput::UseQr), Box::new(|_| Ok(Vec::new()))).unwrap();
        assert!(matches!(qr, SignInStep::Qr { .. }), "{qr:?}");
        let back = step(&hub, Some(SignInInput::Back), Box::new(|_| Ok(Vec::new()))).unwrap();
        assert_eq!(back, browser(false));
        assert!(first.cancelled.load(Ordering::Acquire));
        cancel();
    }
}

static FLOW: Mutex<Option<Arc<Flow>>> = Mutex::new(None);

fn current() -> MutexGuard<'static, Option<Arc<Flow>>> {
    FLOW.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn browser(waiting: bool) -> SignInStep {
    SignInStep::Browser {
        server: "https://messages.google.com".into(), provider: "Google".into(),
        waiting, problem: None,
    }
}

fn waiting_qr() -> SignInStep {
    SignInStep::Qr { link: String::new(), expires: 0, problem: None }
}

fn fresh(hub: &Arc<Hub>) -> Arc<Flow> {
    Arc::new(Flow {
        hub: hub.clone(),
        step: Mutex::new(browser(false)),
        mode: Mutex::new(Mode::Choice),
        running: AtomicBool::new(false),
        ended: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
    })
}

fn replace_flow(held: &mut Option<Arc<Flow>>, hub: &Arc<Hub>) -> Arc<Flow> {
    if let Some(old) = held.take() {
        old.cancelled.store(true, Ordering::Release);
        old.ended.store(true, Ordering::Release);
    }
    let next = fresh(hub);
    *held = Some(next.clone());
    next
}

pub fn step(hub: &Arc<Hub>, input: Option<SignInInput>, adopt: Adopt) -> Result<SignInStep, String> {
    let mut held = current();
    let live = held.as_ref().filter(|flow| !flow.ended.load(Ordering::Acquire)).cloned();
    let flow = match (live, &input) {
        (Some(flow), Some(SignInInput::Back)) => {
            let mode = *flow.mode.lock().unwrap_or_else(|p| p.into_inner());
            if mode == Mode::Qr {
                let reset = replace_flow(&mut held, hub);
                drop(held);
                hub.changed(Change::SignIn { network: Network::Messages, step: browser(false) });
                return Ok(reset.step().clone());
            }
            flow
        }
        (Some(flow), Some(SignInInput::UseQr)) => {
            let mode = *flow.mode.lock().unwrap_or_else(|p| p.into_inner());
            if mode == Mode::Google {
                replace_flow(&mut held, hub)
            } else {
                flow
            }
        }
        (Some(flow), Some(SignInInput::OpenBrowser)) => {
            let mode = *flow.mode.lock().unwrap_or_else(|p| p.into_inner());
            if mode == Mode::Qr {
                replace_flow(&mut held, hub)
            } else {
                flow
            }
        }
        (Some(flow), _) => flow,
        (None, Some(_)) => return Ok(SignInStep::Failed { reason: "That sign-in is over. Start again.".into() }),
        (None, None) => {
            let flow = fresh(hub);
            *held = Some(flow.clone());
            flow
        }
    };
    drop(held);

    match input {
        Some(SignInInput::OpenBrowser) => {
            if flow.running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                *flow.mode.lock().unwrap_or_else(|p| p.into_inner()) = Mode::Google;
                flow.show(browser(true));
                let running = flow.clone();
                if let Err(err) = std::thread::Builder::new().name("messages-link".into()).spawn(move || {
                    let flow = running.clone();
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| running.run_google(adopt))).is_err() {
                        flow.show(SignInStep::Failed { reason: "Messages sign-in stopped unexpectedly. Try again.".into() });
                        flow.ended.store(true, Ordering::Release);
                    }
                }) {
                    flow.show(SignInStep::Failed { reason: format!("Could not start Messages sign-in: {err}") });
                    flow.ended.store(true, Ordering::Release);
                }
            }
        }
        Some(SignInInput::UseQr) => {
            if flow.running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                *flow.mode.lock().unwrap_or_else(|p| p.into_inner()) = Mode::Qr;
                flow.show(waiting_qr());
                let running = flow.clone();
                if let Err(err) = std::thread::Builder::new().name("messages-qr-link".into()).spawn(move || {
                    let flow = running.clone();
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| running.run_qr(adopt))).is_err() {
                        flow.show(SignInStep::Failed { reason: "Couldn't reach Messages to get a pairing code. Try again.".into() });
                        flow.ended.store(true, Ordering::Release);
                    }
                }) {
                    flow.show(SignInStep::Failed { reason: format!("Could not start Messages QR pairing: {err}") });
                    flow.ended.store(true, Ordering::Release);
                }
            }
        }
        Some(SignInInput::UsePhone | SignInInput::Phone { .. } | SignInInput::Code { .. } | SignInInput::Password { .. }) => {
            let shown = flow.step().clone();
            if let SignInStep::Qr { link, expires, .. } = shown {
                return Ok(SignInStep::Qr {
                    link,
                    expires,
                    problem: Some(SignInProblem::Other {
                        reason: "Messages pairs from your phone: open Messages there and scan this code.".into(),
                    }),
                });
            }
            return Ok(shown);
        }
        _ => {}
    }

    let shown = flow.step().clone();
    Ok(shown)
}

pub fn cancel() {
    if let Some(flow) = current().take() {
        flow.cancelled.store(true, Ordering::Release);
        flow.ended.store(true, Ordering::Release);
    }
}

impl Flow {
    fn step(&self) -> MutexGuard<'_, SignInStep> {
        self.step.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn show(&self, step: SignInStep) {
        let held = current();
        if !held.as_ref().is_some_and(|flow| std::ptr::eq(flow.as_ref(), self)) || self.cancelled.load(Ordering::Acquire) { return; }
        self.publish(step);
    }

    // Caller holds the lifecycle lock, so cancel/restart cannot overtake publication.
    fn publish(&self, step: SignInStep) {
        let changed = {
            let mut held = self.step();
            let changed = *held != step;
            *held = step.clone();
            changed
        };
        if changed { self.hub.changed(Change::SignIn { network: Network::Messages, step }); }
    }

    fn cookies(&self) -> Result<HashMap<String, String>, String> {
        if !crate::session::enabled() && std::env::var_os("TINYCLIENT_LOGIN").is_none() {
            return Err("Sign-in is off while TINYCLIENT_DIR is set.".into());
        }
        let mut command = std::process::Command::new(crate::server::login_path().map_err(|e| e.to_string())?);
        protocol::log_policy::share_session(&mut command);
        protocol::launch::no_console(&mut command).arg("messages")
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::inherit());
        let mut child = command.spawn().map_err(|_| "Could not open the Google sign-in browser. Reinstall tinyclient and try again.")?;
        let stdout = child.stdout.take().ok_or("Could not read the Google sign-in result.")?;
        // Drain concurrently: Google cookies can exceed an OS pipe's buffer.
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.take(128 * 1024).read_to_end(&mut bytes).map(|_| bytes)
        });
        let started = Instant::now();
        let status = loop {
            if self.cancelled.load(Ordering::Acquire) || started.elapsed() > Duration::from_secs(16 * 60) {
                let _ = child.kill(); let _ = child.wait(); let _ = reader.join();
                return Err("Google sign-in was cancelled or timed out. Try again.".into());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => {
                    let _ = child.kill(); let _ = child.wait(); let _ = reader.join();
                    return Err("The Google sign-in browser stopped unexpectedly.".into());
                }
            }
        };
        let bytes = reader.join().map_err(|_| "Could not read the Google sign-in result.")?
            .map_err(|_| "Could not read the Google sign-in result.")?;
        if !status.success() {
            return Err(match status.code() {
                Some(1) => "The Google sign-in browser was closed.",
                Some(3) => "Could not open Google sign-in. Install Chrome, Edge or Chromium, check your connection and try again.",
                _ => "Google sign-in did not finish. Try again.",
            }.into());
        }
        #[derive(serde::Deserialize)]
        struct Signed { cookies: HashMap<String, String> }
        serde_json::from_slice::<Signed>(&bytes).map(|signed| signed.cookies)
            .map_err(|_| "The Google sign-in browser returned an unreadable session.".into())
    }

    fn run_google(&self, adopt: Adopt) {
        if self.cancelled.load(Ordering::Acquire) { return; }
        let show = |emoji| self.show(SignInStep::Emoji { emoji });
        let outcome = if scripted() {
            FakeLinker::link(&show, &self.cancelled)
        } else {
            self.cookies().and_then(|cookies| {
                if self.cancelled.load(Ordering::Acquire) { return Err("cancelled".into()); }
                bugle::pair_google(cookies, &show, &self.cancelled)
                    .and_then(|saved| serde_json::to_string(&saved).map_err(|err| err.to_string()))
            })
        };
        self.finish(outcome, adopt);
    }

    fn run_qr(&self, adopt: Adopt) {
        let mut tries = 0u32;
        let outcome = loop {
            if self.cancelled.load(Ordering::Acquire) { return; }
            let show = |link: String, expires: i64| self.show(SignInStep::Qr { link, expires, problem: None });
            let linked = if scripted() {
                FakeLinker::link_qr(tries, &show, &self.cancelled)
            } else {
                bugle::pair_qr(&show, &self.cancelled)
                    .and_then(|saved| serde_json::to_string(&saved).map_err(|err| err.to_string()))
            };
            match linked {
                Ok(session) => break Ok(session),
                Err(_) if self.cancelled.load(Ordering::Acquire) => return,
                Err(why) if why.contains("expired") => {
                    tries += 1;
                    if tries >= QR_TRIES {
                        break Err("The QR code kept expiring. Check that your phone is online and try again.".into());
                    }
                }
                Err(why) => break Err(why),
            }
        };
        self.finish(outcome, adopt);
    }

    fn finish(&self, outcome: Result<String, String>, adopt: Adopt) {
        let held = current();
        if !held.as_ref().is_some_and(|flow| std::ptr::eq(flow.as_ref(), self)) || self.cancelled.load(Ordering::Acquire) { return; }
        let step = match outcome {
            Ok(session) => match adopt(&session) {
                Ok(accounts) => SignInStep::Done { account: accounts.into_iter().next() },
                Err(why) => SignInStep::Failed { reason: format!("Messages paired, but tinyclient could not keep the session: {why}") },
            },
            Err(reason) => SignInStep::Failed { reason },
        };
        self.publish(step);
        self.ended.store(true, Ordering::Release);
    }
}
