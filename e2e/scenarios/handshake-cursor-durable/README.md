# Durable handshake cursor

This is a source component diagnostic, not native GUI or release certification.
Set `E2E_OUT`, optionally `E2E_SRC_DIR` and `CARGO`, then run the driver.
It runs both exact real MLS/FileStore/chatd regressions and rejects missing or ignored tests.

The fixture creates a genuine signed Add and Welcome, then refuses receiver state writes while
allowing notes to be written. On the unfixed base the cursor advances past the unsaved Welcome;
after cold restart the device never joins. The fixed branch reports the storage error and keeps
the cursor, then joins after repair. The same test checks retry without restarting.
The second regression starts with joined devices and refuses saving a received membership commit.
On the base, the cursor skips that unsaved commit and new-epoch ciphertext cannot open after a
restart. The existing fix preserves the cursor and restores decryptability after storage repair,
both on cold restart and in memory. A newly reopened engine decrypts the sender's exact text.
Every fixture is temporary and independent; it uses no real profiles or community server.

Both direct regressions fail behaviorally when transplanted onto the base. An absent test on a
base checkout is setup-incomplete, not valid recorded base failure. This component driver cannot
post `e2e-proof`, certify another platform or replace the required normal-app recorded runs.
Those remain incomplete, so the PR stays draft and this scenario has `release: false`.
