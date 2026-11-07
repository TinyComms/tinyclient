//! Removing the native approval gate must make these real linking exchanges fail their tests.
use super::*;
use protocol::device_approval::{DeviceApproval, Outcome};

fn guarded_link(approval: Arc<dyn DeviceApproval>, expired: bool) -> (Run, Arc<Fake>) {
    guarded_link_cancelling(approval, expired, false)
}

fn guarded_link_cancelling(
    approval: Arc<dyn DeviceApproval>,
    expired: bool,
    cancel_in_prompt: bool,
) -> (Run, Arc<Fake>) {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let tag = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let relay = Arc::new(Fake::default());
    let new = device(
        &format!("guard-new-{tag}"),
        DeviceState::AwaitingApproval {
            code: String::new(),
        },
    );
    let old = device(&format!("guard-old-{tag}"), DeviceState::Ready);
    let new_job = Arc::new(Linking::new(Role::New, quiet()));
    let mut old_job = Linking::new(Role::Old, quiet());
    let (prompt_started, in_prompt) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    old_job.approval = Arc::new(move |reason: &str, cancelled: &dyn Fn() -> bool| {
        let answer = approval.verify(reason, cancelled);
        if cancel_in_prompt {
            prompt_started.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        answer
    });
    if expired {
        old_job.deadline = Instant::now();
    }
    let old_job = Arc::new(old_job);
    let new_thread = {
        let (job, device, relay) = (new_job.clone(), new.clone(), relay.clone());
        std::thread::spawn(move || run_new(&job, &device, relay.as_ref()))
    };
    let wait = Instant::now() + Duration::from_secs(20);
    let code = loop {
        let code = lock(&new_job.state).view.code.clone();
        if !code.is_empty() {
            break LinkCode::parse(&code).unwrap();
        }
        assert!(Instant::now() < wait);
        std::thread::sleep(Duration::from_millis(5));
    };
    let old_thread = {
        let (job, device, relay) = (old_job.clone(), old.clone(), relay.clone());
        std::thread::spawn(move || run_old(&job, &device, relay.as_ref(), &code))
    };
    reaches(&old_job, LinkStage::Confirm);
    say_yes(&old_job, Scope::None);
    if cancel_in_prompt {
        in_prompt.recv_timeout(Duration::from_secs(5)).unwrap();
        lock(&old_job.state).stop = true;
        release.send(()).unwrap();
    }
    let old_result = old_thread.join().unwrap();
    let new_result = new_thread.join().unwrap();
    (
        Run {
            new_result,
            old_result,
            new,
            old,
        },
        relay,
    )
}

#[test]
fn os_refusal_never_releases_a_cross_signature() {
    let approval =
        Arc::new(
            |_: &str, _: &dyn Fn() -> bool| Err("OS authentication was cancelled.".to_owned()),
        );
    let (run, relay) = guarded_link(approval, false);
    assert!(
        run.old_result.is_err(),
        "typed digits cannot replace OS approval"
    );
    assert!(run.new_result.is_err());
    assert!(lock(&relay.registered).is_empty());
    assert!(
        lock(&run.old).notes.mine.is_empty(),
        "no device was cross-signed"
    );
}

#[test]
fn expired_native_approval_never_releases_a_cross_signature() {
    let approval = Arc::new(|_: &str, _: &dyn Fn() -> bool| Ok(Outcome::Verified));
    let (run, relay) = guarded_link(approval, true);
    assert!(
        run.old_result.is_err(),
        "authentication cannot revive an expired link"
    );
    assert!(lock(&relay.registered).is_empty());
}

#[test]
fn verified_owner_can_link_without_history() {
    let approval = Arc::new(|_: &str, _: &dyn Fn() -> bool| Ok(Outcome::Verified));
    let (run, relay) = guarded_link(approval, false);
    assert!(run.old_result.is_ok(), "{:?}", run.old_result);
    assert_eq!(run.new_result, Ok(0));
    assert_eq!(lock(&relay.registered).len(), 1);
}

#[test]
fn cancelling_in_native_prompt_discards_a_late_success() {
    let approval = Arc::new(|_: &str, _: &dyn Fn() -> bool| Ok(Outcome::Verified));
    let (run, relay) = guarded_link_cancelling(approval, false, true);
    assert!(run.old_result.is_err());
    assert!(run.new_result.is_err());
    assert!(lock(&relay.registered).is_empty());
    assert!(lock(&run.old).notes.mine.is_empty());
}

#[test]
fn explicitly_unconfigured_owner_authentication_keeps_link_code_confirmation() {
    let approval = Arc::new(|_: &str, _: &dyn Fn() -> bool| Ok(Outcome::NotConfigured));
    let (run, relay) = guarded_link(approval, false);
    assert_eq!(run.new_result, Ok(0));
    assert!(run.old_result.is_ok());
    assert_eq!(lock(&relay.registered).len(), 1);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_provider_cannot_skip_a_required_native_platform() {
    let approval = Arc::new(|_: &str, _: &dyn Fn() -> bool| Ok(Outcome::Unsupported));
    let (run, relay) = guarded_link(approval, false);
    assert!(run.old_result.is_err());
    assert!(lock(&relay.registered).is_empty());
}
