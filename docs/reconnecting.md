# Reconnecting after a network outage

The shared connection loop used by Signal, Discord, Slack, Teams and Messages
retries temporary failures with exponential backoff and jitter, up to 15 minutes.
An expired or explicitly revoked session stops retrying and keeps its cache.

On Linux, chatd also listens for OS recovery hints. GIO's `GNetworkMonitor`
reports network configuration changes with an available route; its Linux
backends include netlink, so NetworkManager is not required. A system D-Bus
subscription to login1's `PrepareForSleep(false)` covers suspend/hibernate resume.
One GLib event thread serves all accounts and performs no periodic probes.

A hint interrupts an adapter's reconnect wait and resets its accumulated delay.
Attempts stay at least one second apart, including notification bursts. A hint
received during an unsuccessful connection attempt is retained for the following
wait. Ordinary HTTP/command pacing and panic recovery are not interrupted.
Healthy connections are not closed, and hints never mark an account Connected:
only its adapter's successful connection does that. Subsequent failures still
back off, and expired sessions are never revived by a recovery hint.

This detects local route changes and resume, not remote service restoration or
DNS recovery with no OS network event. Missing login1/system D-Bus leaves the
network monitor available. Other platforms retain timed reconnection until they
have a native recovery-event implementation. GIO is a Linux-only dependency;
the Linux GUI packages already supply its runtime libraries.

Recovery produces a journal entry such as:

```text
chatd: Signal: network changed or system resumed; retrying connection
```

After updating chatd, restart the user service once to load the new code:

```sh
systemctl --user restart tinyclient-chatd.service
```

## Validation

The daemon unit suite covers interrupting a simulated 900-second reconnect wait,
a burst of recovery hints, a hint arriving before the wait starts, normal pacing,
stop notification and expired-session behavior.

A Linux integration check exercises actual kernel route notification through the
netlink backend in a disposable network namespace. It explicitly refuses to run
in the same network namespace as PID 1, so it cannot modify host networking:

```sh
cargo test --frozen --release -p chatd --lib --no-run
sudo unshare --net env GIO_USE_NETWORK_MONITOR=netlink \
  target/release/deps/chatd-<test-hash> \
  adapters::recovery::tests::kernel_route_recovery_without_networkmanager_wakes_reconnect \
  --exact --ignored --nocapture
```

Use the test executable path printed by Cargo. The manual test requires `ip`
and permission to create a network namespace; ordinary unit tests do not.

References: [GIO network monitoring](https://docs.gtk.org/gio/class.NetworkMonitor.html)
and [login1 resume signals](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html).
