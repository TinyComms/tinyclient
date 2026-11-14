# Roadmap backlog run — Implementation Plan

> **For the harness:** execute with subagent dispatch — one fresh subagent per task, parallel
> where tasks are independent, exactly as the phases below mark them.

**Goal:** Ship the remaining roadmap work (15 local branches + `unicode-glyphs`) as scoped,
quality-gate-compliant pull requests, after a read-only recon proves which parts are already
on `main`.

**Architecture:** Phase 0 recon (4 explore agents in parallel, read-only) → owner checkpoint
→ external PRs first, then quick wins and grouped build agents in worktrees → interleaved
merges in priority order. Build follows the design in
`docs/superpowers/specs/2026-11-14-roadmap-parallel-design.md`.

**Tech stack:** git (worktrees, bundles, 3-way patch apply), `gh` CLI, Rust workspace
(`cargo test --workspace`, `cargo check -p tinyclient --features gui`), e2e scenarios
(`e2e/scenarios/<id>/scenario.json` + `drive.<platform>` drivers), lab runners per
`e2e/README.md`.

---

## Ground rules (apply to every task)

- The shared checkout stays on `main`; **all writes happen in
  worktrees** (`git worktree add <path> -b <branch> origin/main`).
- `main` is squash-only; agents never push to `main`; only the owner merges and closes
  external PRs.
- Every `feature`/`fix` PR carries `e2e/scenarios/<id>` (drives the real app, fails on
  base, passes on branch — the lab proves it, not our screenshots). `docs`-only PRs are
  exempt (`no_proof_paths`).
- **No PR opens without a pre-PR audit round**: a fresh reviewer subagent audits the
  whole diff (base..head) against the plan, DRY/SSoT/SOLID, repo conventions, test
  quality, and the quality gate. Critical and Important findings are fixed by the
  builder before the PR is created; Minor findings ride in the PR description.
- Commit subjects are plain sentences ("Holding the mic records and letting go stops,
  with push-to-talk on."), never `feat:`-style prefixes.
- "Ready" = `pr-gate` green (today: `template` + `scenario` enforced; `release_proof`
  is `report`). Agents drive lab runs themselves on hosts that are up; macOS/iOS are
  recorded blocked and follow later.
- Before any branch deletion: `git bundle` archive exists under `_archive/branches/`.

---

## Phase 0 — recon (read-only, fully parallel)

### Task 0.1: Archive every branch, pin the lab trigger

**Objective:** Make the irrecoverable recoverable, and record how lab runs are started.

**Step 1:** In the shared checkout run, for each branch B in:
`roadmap-avatars roadmap-comments-polish roadmap-comments-send roadmap-emoji-text
roadmap-home roadmap-ladezeit roadmap-misc-tg roadmap-misc2 roadmap-reactions
roadmap-receipts roadmap-slack-search roadmap-sponsored roadmap-trust-tags roadmap-ttl
roadmap-views-badge unicode-glyphs`:

```bash
mkdir -p _archive/branches
git bundle create _archive/branches/B.bundle B
```

**Step 2:** Read `e2e/README.md` to the end and `scripts/e2e/` listing. Write the exact
command(s) an agent uses to start a lab run for a PR into this plan's "Lab trigger"
appendix below. Do not invent a command; if the mechanism is a GitHub workflow, record
its `gh workflow run`/`gh run watch` form.

**Step 3:** Verify each bundle: `git bundle verify _archive/branches/B.bundle` — expected:
`ok` for all 16.

### Tasks 0.2–0.5: Recon batches (4 explore agents, dispatched in one message)

**Objective:** One verdict per branch: `SHIPPED` / `SHIPPED-SUPERSEDED` / `PARTIAL` /
`OPEN`, with evidence.

**Batches:**

- **R1:** `roadmap-avatars`, `roadmap-home`, `roadmap-misc-tg`, `roadmap-misc2`
- **R2:** `roadmap-receipts`, `roadmap-views-badge`, `roadmap-ttl`, `roadmap-sponsored`
- **R3:** `roadmap-comments-send`, `roadmap-comments-polish`, `roadmap-emoji-text`,
  `roadmap-reactions`
- **R4:** `roadmap-trust-tags`, `roadmap-ladezeit`, `roadmap-slack-search`,
  `unicode-glyphs`

**Agent prompt template (same for all four, swap the batch):**

```
Read-only analysis of branch drafts against current main in the shared checkout.
Do not modify anything. For each branch in <BATCH>:

1. Note its tip subject: git log -1 --format='%s' <branch>
2. Find its merge base: git merge-base main <branch>
3. List its own changes: git diff <merge-base>..<branch> --stat
4. Pick 3–8 distinctive symbols (function/test names, settings constants, fixture
   strings) from the diff — real code identifiers, not comments.
5. Probe main for each: git grep -n '<symbol>' main -- crates/ docs/
6. Cross-check main's history: git log main --oneline -200 and match topic words
   against the wave squashes (#155 "search pane asks a network", #232 "Telegram wave
   3.6: push-to-talk") and the release commits 0.2.63–0.2.69.
7. Check for vendor churn noise: if the diff is dominated by vendor/gpui_ce_* or
   release-era drift, note the branch as an old draft (this matters for
   SHIPPED-SUPERSEDED).

Verdict rules:
- SHIPPED: every distinctive behaviour's symbols exist on main.
- SHIPPED-SUPERSEDED: the feature is on main in an evolved form that improves on the
  draft (compare the two implementations for the key file(s) before claiming this).
- PARTIAL: name exactly which behaviours are missing, each with file + missing symbol
  evidence, and which files the remainder touches.
- OPEN: nothing found on main.

Return one table: Branch | Verdict | Evidence (found / missing symbols) | Remaining
scope (behaviours + files) | Suggested group (G1 GUI-only / G2 telegram-chatd /
G3 infra-slack) | Notes. No prose beyond the table.
```

### Task 0.6: Consolidate and checkpoint

**Objective:** One backlog table for the owner; dispatch waits for his go.

**Step 1:** Merge the four agent tables; resolve conflicts between verdicts by re-running
the probe on the disputed symbol.

**Step 2:** Append the final table under "Backlog verdicts" at the bottom of this plan.

**Step 3:** Present to the owner: table + proposed group assignments + what gets deleted.
Build starts only after his go.

---

## Phase 1 — external PRs first (parallel with Phase 0)

### Task 1.1 and 1.2: Review #253 and #254 (one general agent each, parallel)

**Objective:** A review the owner can act on: findings inline, recommendation in the
thread. Agents never merge or close.

**Agent brief (same shape for both, swap the number):**

```
Work in a worktree: git fetch origin <headRefName>; git worktree add
C:/<worktree-root>/pr<N> <headRefName>.
1. Read docs/pr-review.md and follow it — that is the repo's review format.
2. Read the diff: gh pr diff <N>; read the touched files in the worktree.
3. Run the tests that cover the touched area (cargo test -p chatd <matching test
   substrings>), and cargo check -p tinyclient --features gui if gui files changed.
4. Post inline findings on the PR (gh api or gh pr review --request-changes/comment
   with --body), following docs/pr-review.md: fix-or-argue, never resolve silently.
5. Check the gate requirements: does the diff need an e2e scenario per gate.json
   (types feature/fix)? Does it have one? Which platforms does pr-gate plan?
6. Return: verdict (approve / request-changes / needs-owner-decision), the reason,
   test results, and whether an e2e scenario is missing.
Do NOT merge, close, or push anything.
```

**Step after both return:** present verdicts to the owner. He merges or closes.

### Task 1.3: Absorb the outcome

**Step 1:** `git fetch origin --prune`; `git switch main; git merge --ff-only origin/main`.
**Step 2:** If a PR merged: note its file set — the groups' extract work rebases over it.

---

## Phase 2 — quick wins (start after Task 1.3; parallel with each other)

### Task 2.1: Docs-sync PR (`docs` type, no proof needed)

**Files:** `README.md`, plus this plan and the design doc ride along.

**Step 1:** Worktree: `git worktree add C:/<worktree-root>/docs-sync -b docs/roadmap-2026-11 origin/main`.

**Step 2:** Fix the proven-stale facts in `README.md`:
- Line 64: "Latest tagged release: [v0.2.35]…" → link `releases/latest` and drop the
  "plus one commit" claim (verify `git log origin/main -1` before writing it).
- Line 101: remove the "**Recording a voice message** | … not built" row; recording
  shipped in 0.2.67 (#232). Move one sentence about recording (push-to-talk optional)
  into the Discord/Telegram rows of the "What works today" table if not already there.
- The "Search on the network" row: verify with `git grep -n 'can.search\|Search' main --
  crates/protocol/src` which networks answer search today; correct the row.
- Anything else the Phase 0 tables proved stale: fix in the same PR.

**Step 3:** PR type `docs`, one checkbox, no scenario. Owner merges.

### Task 2.2: Issue #234 — Tiny E2EE reactions refused (TDD, `fix` type)

**Objective:** Reactions work in an encrypted Tiny conversation; regression test first.

**Step 1 (diagnose):** Worktree off `origin/main`, branch `fix/tiny-e2ee-reactions`.
Read `crates/chatd/src/adapters/tiny.rs` (reaction send path), `crates/chatd/src/
adapters/tiny/messages.rs`, `crates/chatd/src/adapters/tiny/e2ee/`. Hypothesis to check
first: the sealed-send path refuses or drops reactions as unsupported content, while the
plain path accepts them.

**Step 2 (failing test):** Add a test beside the existing Tiny round-trip tests
(`crates/chatd/src/adapters/tiny/live/tests.rs` has the live socket round-trip shape):
react on a message in an encrypted conversation, assert the reaction lands and comes
back. Run: `cargo test -p chatd tiny -- --nocapture` — expected: the new test FAILS on
unmodified main (that is the bug).

**Step 3 (fix):** Minimal change on the refusal path. Re-run: expected PASS, whole
`cargo test -p chatd` green.

**Step 4:** Scenario `e2e/scenarios/tiny-e2ee-reaction/` (`scenario.json` platforms
windows+linux to start, `needs: ["server"]`, drivers following
`e2e/scenarios/keycap-reaction/` shape — it is the closest existing reaction scenario).
Base fails, branch passes.

**Step 5:** PR type `fix`, template filled, lab run started. Owner merges.

---

## Phase 3 — grouped build (dispatch after the Task 0.6 checkpoint)

### The Topic-PR recipe (applies to every confirmed topic below)

1. **Worktree + fresh branch** (never the old draft): `git worktree add
   C:/<worktree-root>/<id> -b feat/<id> origin/main`.
2. **Extract, don't rebase:** for each file in the recon remainder, generate the draft's
   delta and 3-way apply only the remainder hunks:
   `git diff <merge-base>..roadmap-<topic> -- <file> > _archive/patches/<id>-<file>.patch`
   then `git apply --3way` inside the worktree; resolve; drop draft-only noise
   (vendor/**, formatting, unrelated fixes).
3. **Tests:** `cargo test --workspace` green; `cargo check -p tinyclient --features gui`
   green. New behaviour gets a unit test beside the existing ones first (TDD).
4. **Scenario:** `e2e/scenarios/<id>/` per `e2e/README.md` — `scenario.json` lists the
   platforms the diff reaches (gate adds them anyway), `drive.py` +
   `drive.<platform>` drivers, `steps.jsonl` shape as the README shows.
5. **Pre-PR audit round (mandatory):** the builder stops after green tests + scenario
   and reports base SHA, head SHA, diff summary, test evidence. The coordinator
   dispatches a fresh reviewer subagent (read-only, reviews the diff itself, no
   sub-dispatch) covering: plan alignment; DRY / SSoT / SOLID — single source of truth
   for each fact, no duplicated logic, no speculative abstraction; repo conventions
   (commit subjects, no reformatting, test style); error handling and edge cases;
   scenario quality. Findings are graded Critical / Important / Minor. The builder
   fixes Critical and Important, re-runs the touched tests, and answers or pushes back
   on each finding with reasoning. Only a clean round opens the PR.
6. **PR:** from `.github/pull_request_template.md`, exactly one type checked, platforms
   per diff, Proof section names `e2e/scenarios/<id>`. Start the lab run (see "Lab
   trigger" appendix). Minor audit findings are listed in the PR description.
7. **After merge:** owner merges; agent deletes the extract branch (bundle exists),
   drafts stay until the whole run ends.

### Task 3.1: G1 — rows and markers (GUI only, no `crates/chatd` changes)

Topics (as confirmed by checkpoint): avatars, views-badge, misc2 mute bell, receipts
(window side), home. Files: `crates/tinyclient/src/gui/sidebar*`, `gui/conversation/
draw.rs`, `gui/components/avatar.rs`. One agent, one worktree, one PR per topic (small
PRs merge cleanest), built sequentially inside the group.

### Task 3.2: G2a — telegram chatd, comments and reactions

Topics: comments-send, comments-polish, emoji-text, reactions, plus every
`telegram/sync.rs` change G1's topics need. Files: `crates/chatd/src/adapters/telegram*`,
`crates/protocol/`. One agent, PRs per topic, sequential.

### Task 3.3: G2b — telegram chatd, message lifecycle

Topics: ttl, sponsored, trust-tags, misc-tg. Same files as G2a — dispatched after G2a's
PRs are ready to merge, rebased on whatever main then is.

### Task 3.4: G3 — infra and Slack

Topics: ladezeit (boot-wait prune), slack-search. Files: `crates/chatd/src/{serve,store,
server}.rs`, `crates/chatd/src/adapters/slack*`, the search pane in `crates/tinyclient/`.
One agent, PRs per topic.

### The merge loop (continuous, owner + agents)

After the owner merges any PR: `git fetch origin --prune`; each still-building worktree
runs `git rebase origin/main` on its current branch and re-runs its tests before marking
the next PR ready. Conflicts surface here, small and one at a time.

### Task 3.5: End-of-run cleanup

**Step 1:** All confirmed topics merged (or consciously dropped at the checkpoint).
**Step 2:** Delete the shipped drafts: `git branch -D roadmap-<topic>` (bundles already
exist in `_archive/branches/`). Keep `unicode-glyphs` only if recon proved unshipped
content still lives there; otherwise bundle-and-delete it too.
**Step 3:** Final table appended below; run closed.

---

## Backlog verdicts

*(final — R1–R4 complete)*

| Branch | Verdict | Evidence (key symbols on main) | Remaining | Group | Shipped via |
|---|---|---|---|---|---|
| roadmap-receipts | SHIPPED | `the_dialog_says_whether_they_read_my_last_word_and_a_groups_read_flips_live` (tests/telegram.rs:1782), outbox steer (fake.rs:1236), `read_outbox_max_id` (fake.rs:239), `preview_mine`/`seen` (store.rs:147), ✓✓ test (draw.rs) | none | G2 | `e8e7e89` (#175) |
| roadmap-views-badge | SHIPPED | `views_short` + test (draw.rs:2131/2173), `views_line` (draw.rs:1837), `views: Option<u32>` (protocol/message.rs:380), `comment_unread` clear (telegram.rs:674) | none | G2 | `96e1742` (wave 2) + `e8e7e89` |
| roadmap-ttl | SHIPPED-SUPERSEDED | `ttl_period` (content.rs:499), `expires` column + index (store.rs:189/882), `hub::expiring` (hub.rs:287); main adds `preview_expires` (SCHEMA 10) beyond the draft | none — main evolved past it | G2 | `e8e7e89` (#175) + preview-expiry work |
| roadmap-sponsored | SHIPPED | `Request::FollowLink` (protocol/lib.rs:1318), `sponsored: bool` (protocol/message.rs:269), `random_of` + test (sync.rs:1374), `ClickSponsoredMessage` (sync.rs:1369), GUI follow (draw.rs:1041) | none | G2 | `e8e7e89` (#175) |

Shared R2 note: every branch is a stacked draft ~95% vendor/gpui_ce_windows drift + common
baseline (PTT recorder, Slack search, wave-2 fixups — shipped separately via #232 and the
Slack work); only tip commits are each branch's own topic.

| Branch | Verdict | Evidence (key symbols on main) | Remaining | Group | Shipped via |
|---|---|---|---|---|---|
| roadmap-comments-send | SHIPPED-SUPERSEDED | `comment_target` (sync.rs:984), `comments_thread` (sync.rs:1015), comment tests (tests/telegram.rs:1097–1142), cloud drafts (fake.rs:634–774), `folder_pins` (store.rs:1528), VoiceNote send (send.rs:47) | none — only `blobs::path_of_file`/`name_is_dated` superseded by id-named blobs | G2 | #155 `96e1742` |
| roadmap-comments-polish | SHIPPED-SUPERSEDED | comment-reaction/draft tests (tests/telegram.rs:1174–1230), `echo_thread` (sync.rs:1022), `comment_unread` (sync.rs:1043); payload incl. Slack search + recorder | none | G2 | #175 `e8e7e89` |
| roadmap-emoji-text | SHIPPED-SUPERSEDED | `text_emoji_ids` (content.rs:376), `text_emoji` (sync.rs:861), premium-emoji tests (tests/telegram.rs:970, content.rs:658), `fetch_emoji` (sync.rs:833) | none | G2 | #155 `96e1742` |
| roadmap-reactions | SHIPPED-SUPERSEDED | `reaction_emoji_ids` (content.rs:362), `reaction_emoji` (sync.rs:850), custom-emoji chip test (tests/telegram.rs:941). #233 covers only the keycap sub-topic | none | G2 | #155 `96e1742` |
| roadmap-trust-tags | SHIPPED | `FAKE = 7006`/"Support Desk" fixture (fake.rs), `the_networks_own_login_code_chat_notifies_like_a_direct_message` (notify/tests.rs:118), `Mark::Fake` + `Warning::Fake` (telegram/info.rs:47), 777000-style login-code chat as DM | none — main's fake dialog evolved further | G1 | wave squashes + #232 + Slack squash |
| roadmap-ladezeit | SHIPPED | `AFTER_BOOT` (server.rs:285, evolved with `EXPIRY_EVERY` cap), `prune_due` (hub.rs:293), prune tests (tests/telegram.rs:619/638), `fetch_page`/`ask_page`/`record_page` (sync.rs:422–459) | none | G2 | `96bd9e1`, `96e1742`, #232 |
| roadmap-slack-search | SHIPPED | `search_messages` (slack/search.rs:20), `SEARCH_PATIENCE` (slack.rs:47), refusal wording (slack.rs:182), tests (tests/slack.rs:2224/2257), README:102 | none — main is a strict superset (`network_searches(&Capabilities)` gate) | G3 | Slack-search squash |
| unicode-glyphs | SHIPPED | pagination (sync.rs:422–459 ← 3030e3a), AFTER_BOOT prune (server.rs:285 ← 57b0f93), held-back wire (`TINYCLIENT_TELEGRAM_HOLD`, fake.rs:807), recorder `pitch()` evolved (mediad/rec.rs:173) | none — all three +258 pieces mapped and found | G2 | #232 + telegram-chatd commits |
| roadmap-avatars | SHIPPED | `Want.watched` (content.rs:26), `Job::Look`/`rows_back` (files.rs:118/228), 4-in-flight tests (tests/telegram.rs:539/570) | none — main's `rows_back` evolved (+`counted`, `row_shown`) | G2 | wave-2/#155-era follow-ups |
| roadmap-home | SHIPPED | `Prefs.telegram_folder` (prefs.rs:144/250/311), `remember_telegram_folder` (sidebar.rs:206), `Look.pin` (sidebar/chat.rs:29), `filed.open` freeze (filing.rs:55), folder/open/archive tests (tests/telegram.rs:396–444) | none — main adds relaunch-restore tests beyond the draft | G1 | wave-2/#155-era follow-ups |
| roadmap-misc-tg | SHIPPED | `Store::landing` (store.rs:1577), `Topics { landing }` (protocol/lib.rs:1142), forum-landing test (tests/telegram.rs:483), `BadgeLook::Quiet` + muted-rail test (:467) | none | G2 | wave-2/#155-era follow-ups |
| roadmap-misc2 | **PARTIAL** | Found: `Capabilities.search` + `network_searches(&Capabilities)` (side.rs:104), all shared-stack symbols. **Missing:** (a) `mute_mark` bell-off on muted Telegram rows (components.rs, sidebar/chat.rs; spec: docs/product/polish/03-telegram.md §4 row 6); (b) Discord forum opens on newest-activity post, posts off the tree (discord/ready.rs, store.rs `landing` Discord branch, server.rs, tests/discord.rs, fixtures/discord/ready.json) | (a) → G1, (b) → G2 | G1+G2 | clean diff, newer base, no vendor churn — only the two tip behaviours are unshipped |

## Lab trigger

Driver: `python e2e/lab/lab.py` (hosts in `e2e/lab/hosts.json`; supports `--platforms a,b`;
upload to the `TinyComms/e2e-proof` release is mandatory — `--no-upload` without `--no-post`
is refused; `pr-gate.py plan <number>` in `.github/scripts` names the required platform plan).
An agent runs `python e2e/lab/lab.py --help` before its first run and uses the PR form so the
statuses land on the head SHA (`e2e-lab/<platform>`, then `e2e-proof` when the plan is green).
macOS/iOS hosts are recorded blocked in `docs/quality-gate.md`; their platforms follow later.
