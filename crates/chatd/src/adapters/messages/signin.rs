//! Google account sign-in in a browser, followed by an emoji confirmation on the phone.

use super::{bugle, fake::FakeLinker, scripted};
use crate::hub::Hub;
use protocol::{AccountKey, Change, Network, SignInInput, SignInStep};
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub type Adopt = Box<dyn Fn(&str) -> Result<Vec<AccountKey>, String> + Send + Sync>;

struct Flow {
    hub: Arc<Hub>,
    step: Mutex<SignInStep>,
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

pub fn step(hub: &Arc<Hub>, input: Option<SignInInput>, adopt: Adopt) -> Result<SignInStep, String> {
    let mut held = current();
    let flow = match held.as_ref().filter(|flow| !flow.ended.load(Ordering::Acquire)) {
        Some(flow) => flow.clone(),
        None if input.is_some() => return Ok(SignInStep::Failed { reason: "That sign-in is over. Start again.".into() }),
        None => {
            let flow = Arc::new(Flow {
                hub: hub.clone(), step: Mutex::new(browser(false)), running: AtomicBool::new(false),
                ended: AtomicBool::new(false), cancelled: AtomicBool::new(false),
            });
            *held = Some(flow.clone());
            flow
        }
    };
    drop(held);
    if matches!(input, Some(SignInInput::OpenBrowser)) &&
        flow.running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        flow.show(browser(true));
        let running = flow.clone();
        if let Err(err) = std::thread::Builder::new().name("messages-link".into()).spawn(move || {
            let flow = running.clone();
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| running.run(adopt))).is_err() {
                flow.show(SignInStep::Failed { reason: "Messages sign-in stopped unexpectedly. Try again.".into() });
                flow.ended.store(true, Ordering::Release);
            }
        }) {
            flow.show(SignInStep::Failed { reason: format!("Could not start Messages sign-in: {err}") });
            flow.ended.store(true, Ordering::Release);
        }
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

    fn run(&self, adopt: Adopt) {
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
