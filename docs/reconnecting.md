# Reconnecting after a network outage

Discord, Slack, Teams, Telegram, Signal and Messages share one connection loop
(`keep_connected` in `crates/chatd/src/adapters/mod.rs`). A connection that never
holds doubles its pause from 1 second up to 15 minutes, plus jitter of up to a
quarter of that pause. A connection that held for more than 60 seconds retries
in about a second; repeated drops close together double that pause, and the
drop count decays. `Failure::SignedOut` stops retrying, marks the account
`Expired`, and leaves the cache and the keychain item in place.

Tiny does not use that loop. The live socket (`adapters/tiny/live.rs`) follows
the server's ladder: 0.5 seconds, then double, capped at 10 seconds. A socket
that held for 60 seconds starts from 0.5 seconds again. The close code chooses
resume, start over, sign in again, slow down at the 10 second cap, or, for an
older server with no live socket, ask again after 5 minutes. A session the
server ended, or a member who was removed, stops and stays `Expired`. Two
devices replacing each other wait until the user opens that server again.

On Linux, chatd also listens for OS recovery hints. GIO's `GNetworkMonitor`
reports network configuration changes with an available route; its Linux
backends include netlink, so NetworkManager is not required. A system D-Bus
subscription to login1's `PrepareForSleep(false)` covers suspend/hibernate resume.
One GLib event thread serves all accounts and performs no periodic probes.

A hint interrupts whichever reconnect wait is running and resets that loop's
accumulated delay. On the shared loop the next attempt starts at least one
second after the previous attempt started, including a burst of hints. On Tiny
that floor is 0.5 seconds. A hint received during an unsuccessful connection
attempt is retained for the following wait. Ordinary HTTP/command pacing and
panic recovery are not interrupted. Healthy connections are not closed, and
hints never mark an account Connected: only its adapter's successful connection
does that. Subsequent failures still back off, and expired sessions are never
revived by a recovery hint.

A dropped Tiny or Discord *call* is a different budget. It is in `docs/architecture.md`
under chatd and mediad, not in this loop.

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
