//! OS hints wake reconnect backoff; they never declare an account connected.
//! Linux uses GIO's network monitor (netlink without NetworkManager) and login1
//! resume signals. One event loop serves all accounts without periodic probes.

use super::Halt;
#[cfg(any(target_os = "linux", test))]
use crate::hub::lock;
#[cfg(any(target_os = "linux", test))]
use std::sync::atomic::Ordering;

#[cfg(any(target_os = "linux", test))]
pub(super) fn hint(stop: &Halt) {
    // Share the waiter's mutex so a notification cannot fall between checking
    // the generation and entering the condvar wait.
    let _waiting = lock(&stop.waiting);
    stop.recovery.fetch_add(1, Ordering::Relaxed);
    stop.wake.notify_all();
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use gio::prelude::*;
    use std::sync::{Arc, Mutex, Once, OnceLock, Weak};

    static ACCOUNTS: OnceLock<Mutex<Vec<Weak<Halt>>>> = OnceLock::new();
    static START: Once = Once::new();

    fn changed() {
        let mut accounts = lock(ACCOUNTS.get_or_init(Mutex::default));
        accounts.retain(|account| {
            let Some(account) = account.upgrade() else { return false; };
            hint(&account);
            true
        });
    }

    pub(super) fn watch(stop: &Arc<Halt>) {
        {
            let mut accounts = lock(ACCOUNTS.get_or_init(Mutex::default));
            accounts.retain(|account| account.strong_count() != 0);
            accounts.push(Arc::downgrade(stop));
        }
        START.call_once(|| {
            if let Err(err) = std::thread::Builder::new().name("network-recovery".into()).spawn(run) {
                tracing::warn!("chatd: could not watch network recovery: {err}");
            }
        });
    }

    fn run() {
        let context = gio::glib::MainContext::new();
        let result = context.with_thread_default(|| {
            let monitor = gio::NetworkMonitor::default();
            let handler = monitor.connect_network_changed(|_, available| {
                if available { changed(); }
            });
            // Keep the system bus and its subscription alive with this loop.
            // Network recovery still works when login1/system D-Bus is absent.
            let bus = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).ok();
            let subscription = bus.as_ref().map(|bus| bus.signal_subscribe(
                Some("org.freedesktop.login1"), Some("org.freedesktop.login1.Manager"),
                Some("PrepareForSleep"), Some("/org/freedesktop/login1"), None,
                gio::DBusSignalFlags::NONE,
                |_, _, _, _, _, args| {
                    if args.get::<(bool,)>() == Some((false,)) { changed(); }
                },
            ));
            gio::glib::MainLoop::new(Some(&context), false).run();
            if let (Some(bus), Some(subscription)) = (bus, subscription) { bus.signal_unsubscribe(subscription); }
            monitor.disconnect(handler);
        });
        if let Err(err) = result { tracing::warn!("chatd: could not watch network recovery: {err}"); }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn watch(stop: &std::sync::Arc<Halt>) { linux::watch(stop); }

#[cfg(not(target_os = "linux"))]
pub(super) fn watch(_: &std::sync::Arc<Halt>) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use gio::prelude::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "requires an isolated network namespace; see docs/testing-chatd.md"]
    fn kernel_route_recovery_without_networkmanager_wakes_reconnect() {
        assert_eq!(std::env::var("GIO_USE_NETWORK_MONITOR").as_deref(), Ok("netlink"));
        assert_ne!(std::fs::read_link("/proc/self/ns/net").unwrap(), std::fs::read_link("/proc/1/ns/net").unwrap(),
            "never modify the host network: run this test under unshare --net");
        let context = gio::glib::MainContext::new();
        context.with_thread_default(|| {
            let monitor = gio::NetworkMonitor::default();
            assert_eq!(monitor.type_().name(), "GNetworkMonitorNetlink");
            let stop = Arc::new(Halt::default());
            let recovery = stop.clone();
            let handler = monitor.connect_network_changed(move |_, available| {
                if available { hint(&recovery); }
            });
            for args in [vec!["link", "add", "testnet0", "type", "dummy"],
                vec!["link", "set", "testnet0", "up"],
                vec!["addr", "add", "192.0.2.2/24", "dev", "testnet0"],
                vec!["route", "add", "default", "dev", "testnet0"]] {
                assert!(std::process::Command::new("ip").args(args).status().unwrap().success());
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while stop.recovery.load(Ordering::Relaxed) == 0 {
                while context.pending() { context.iteration(false); }
                assert!(Instant::now() < deadline, "kernel route notification never reached the reconnect wakeup");
                std::thread::sleep(Duration::from_millis(1));
            }
            monitor.disconnect(handler);
        }).unwrap();
    }
}
