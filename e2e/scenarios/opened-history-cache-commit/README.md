# Opened history before receive-state persistence

This component scenario runs real MLS, FileStore and the Tiny receive/cache path
inside `chatd`. It checks a writable-cache control and a failed cache append,
storage repair, and cold restart with unchanged keys. Temporary synthetic data is
used throughout. A failed cache commit must refuse the read without spending its
receive key; a later successful read must survive reopening.

The old daemon does not implement this proof mode. Its missing marker is not
independent behavioral proof: the direct receive/cache failure regression on the
base supplies the before-fix evidence.

The scenario is `release: false`. It supplies no native GUI/mobile recording,
arbitrary power-loss proof, or atomic transaction across cache and state files.
Tests separately exercise an older state snapshot alongside a durable cache and
state-store write failures. Existing lenient state-save semantics remain: replay
is rejected in memory while a failed state save awaits retry, but a restart from
a lagging snapshot does not guarantee rejection of the same ciphertext under a
different server row ID. Neither partial-write nor sync-error injection is
certified by this scenario. Native lab evidence remains required before merge.
