# The keys of an encrypted call, in its log

A headless probe, not a window: it opens no GUI and no audio device, and it does not prove the
late-frame hold, the proposal retry or the recovery counting. Those are unit tests in
`crates/mediad/src/e2ee/tests.rs`.

`drive.py` runs one test, `an_encrypted_join_logs_when_its_keys_came_and_what_the_wait_cost` in
`crates/mediad/tests/tiny.rs`. It starts two real mediad processes of this build against the
test's own SFU and MLS external sender, has both talk for 400 ms before the group has keys, then
commits the group and waits until each hears the other. It passes only when each mediad's own
`logs/mediad.log` has both lines:

```
call join stage=keys_ready since_offer_ms=…
mediad: e2ee: keys ready; frames dropped while there were none sent=… heard=…
```

with `sent` above zero. A build from before the change has neither line and not the test, so the
filter matches nothing and the driver fails there.

It needs the source tree and `cargo` on the lab host (`CARGO` names another one).
