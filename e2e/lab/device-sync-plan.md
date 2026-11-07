# Device sync lab integration plan

**Goal:** Execute the accepted coordinated IPC scenario through the lab without overstating native coverage.

**Architecture:** A shared coordinator session runs the scenario once per source commit and label. Each supported platform consumes its own recording through the existing lab validators. Private configuration pins fixture and binary inputs; native adapters remain an explicit capability boundary.

**Constraints:** No host actions during implementation. No change to release eligibility, recording thresholds, or product/drivers. Only exact source markers qualify. Existing evidence is not reused.

- [x] Write failing tests for coordinated dispatch, shared execution, pinned configuration, extended arguments, provenance and evidence path validation, incomplete base rejection, timeout cleanup and unsupported native platforms.
- [x] Implement the bounded session and route coordinated scenarios through it before one-app adapter actions.
- [x] Document private configuration, manifest/API, native integration boundary and remaining recording constraints.
- [x] Run focused tests and the complete Python lab suite; inspect diff, commit scoped files locally and report evidence.

**Review focus:** An infrastructure failure must never serve as the expected base regression; missing platform evidence cannot be borrowed; outputs cannot escape their run directory; shared participants cannot race one fixture; cleanup failure must prevent certification.

**Verification:** 107 lab tests passed; 32 scenario tests ran with 24 passed and 8 Linux-only skips on Windows. Native Windows owned-child tests verified process identity rejection and matching cleanup. Existing lab ResourceWarnings remain in test_lab.py and test_mac.py. These are harness checks, not new desktop/mobile runtime proof.

**Completion limit:** Timeout cleanup covers registered app, recorder, fixture and remote participants. Unregistered build/conversion descendants have no whole-tree guarantee. Native platform recording proof is still required and release eligibility is unchanged.
