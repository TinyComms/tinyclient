# Long-conversation sealed history

This component scenario runs the production `Cache::put` / `Cache::load` code in
`chatd`: it appends 20,001 genuinely sealed records, closes the cache, reopens it,
and requires every message to remain readable. It uses fresh temporary files.

The regression can also be run with:

```sh
cargo test -p chatd --lib encrypted_history_beyond_twenty_thousand_messages_survives_restart
```

With the old insertion cutoff retained, that test fails at `m20000`, the first
record beyond the 20,000-message limit. Removing the cutoff makes it pass. The
old daemon also lacks this scenario's proof mode; its missing success marker is
not independent evidence of the history bug.

This is **not native GUI or release proof**. There is no recording, MLS handshake,
message rendering, or mobile runtime in this scenario. Native lab coverage is
still needed to satisfy the full recording contract. The scenario is deliberately
`release: false` and does not certify recovery of history already lost by an older
client.
