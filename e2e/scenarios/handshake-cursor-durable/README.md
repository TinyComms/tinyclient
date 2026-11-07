# Durable handshake cursor

This is a source component diagnostic, not native GUI or release certification.
Set `E2E_OUT`, optionally `E2E_SRC_DIR` and `CARGO`, then run the driver.
It runs the exact real MLS/FileStore/chatd regression and rejects missing or ignored tests.

The fixture creates a genuine signed Add and Welcome, then refuses receiver state writes while
allowing notes to be written. On the unfixed base the cursor advances past the unsaved Welcome;
after cold restart the device never joins. The fixed branch reports the storage error and keeps
the cursor, then joins after repair. The same test checks retry without restarting.
Every fixture is temporary and independent; it uses no real profiles or community server.

The direct regression fails behaviorally when transplanted onto the base. An absent test on a
base checkout is setup-incomplete, not valid recorded base failure. This component driver cannot
post `e2e-proof`, certify another platform or replace the required normal-app recorded runs.
Those remain incomplete, so the PR stays draft and this scenario has `release: false`.
