# Shipping the roadmap backlog: recon first, build in file-disjoint groups

Date: 2026-11-14. Repo: tinyclient. Working state: `main` at `9428915` (0.2.69), clean.

## The problem

The roadmap work from 2–4 November lives as 15 local branches plus `unicode-glyphs`, all
with merge bases 98–138 commits behind current `main` (six-plus releases of drift, plus
`vendor/gpui_ce_*` churn inside the drafts). History shows the waves
(#155 wave 2, #232 wave 3.6) absorbed whole topics, sometimes in an evolved form that
superseded the branch draft (proof: `roadmap-ptt` and `roadmap-voice` were drafts of what
#232 shipped; `roadmap-search-flag` was byte-identical to the merged wave 2 tip). Nobody
knows which of the 15 branches still hold unshipped work. Meanwhile:

- `README.md` on `main` is stale: it announces v0.2.35 and claims voice-message recording is
  not built, which shipped in 0.2.67. The closed `docs/roadmap-sync` PR (#73) tried this
  job and its snapshot is outdated too.
- Issue #234 is open: Tiny E2EE reactions are refused — a real bug in shipped functionality.
- PRs #254 and #253 (external contributors) sit unreviewed.

Running eight build agents on eight topics at once is rejected on evidence, not taste: the
branches' changed-file sets share `crates/chatd/src/adapters/telegram.rs`,
`crates/tinyclient/src/gui/sidebar.rs`, `crates/tinyclient/src/gui/composer.rs` and
`crates/chatd/src/store.rs` across nearly every topic. Full parallelism buys pseudo-speed:
eight open pull requests that each rebase against the last one merged.

## What this builds

A three-phase run, recon before build, build grouped by file overlap, merge serial.

### Phase 0 — recon (parallel, read-only)

4–6 explore agents, no write access, one per batch of branches. Each verdicts one branch as:

- **SHIPPED** — content already on `main` (branch gets deleted, no PR),
- **PARTIAL** — some content shipped, a named remainder is not (PR scoped to the remainder),
- **OPEN** — nothing shipped (full topic PR).

Method per branch: three-dot diff against the merge base, per-file comparison against
`main`, key-symbol probes (grep for the branch's distinctive functions/tests in `main`),
cross-check against the wave squash commits (#155, #232) and release notes 0.2.63–0.2.69.
Also: map `unicode-glyphs`' +258 lines over `roadmap-voice` (Telegram sync pagination
`fetch_page`/`ask_page`/`record_page`, the `AFTER_BOOT` prune wait) to their topics.

Deliverable: one backlog table — topic, verdict, remaining scope, file list, proposed group.

### Phase 1 — build (parallel, grouped by file overlap)

One worktree per group, agent per group, only topics with a real remainder from Phase 0:

- **G1 rows and markers** (GUI only): avatars, views-badge, misc2 mute bell, receipts ✓✓
  (window side), home — `gui/sidebar*`, `gui/conversation/draw.rs`, `gui/components/
  avatar.rs`. G1 agents do not touch `crates/chatd`; whatever a G1 topic needs on the
  daemon side belongs to the G2 sub-wave that carries it.
- **G2 Telegram chatd**: comments-send, comments-polish, emoji-text, reactions, ttl,
  sponsored, trust-tags, misc-tg, and every Telegram-adapter change a G1 topic needs
  (read-state sync included) — `chatd/adapters/telegram*`, `protocol`. Merged as two
  sub-waves (G2a comments/emoji/reactions, then G2b ttl/sponsored/trust-tags/misc-tg)
  because even inside the group the same adapter files collide.
- **G3 infra and Slack**: ladezeit (boot-wait prune), slack-search — `chatd/{serve,store,
  server}.rs`, `adapters/slack*`, the search pane.

Every pull request satisfies the quality gate: its own `e2e/scenarios/<id>` driving the
real app, failing on `main` without the change; PR template with one type checked. GLM's
advisory review (#238) runs on every PR; findings get fixed or answered in the thread.

### Phase 2 — cross-cutting (parallel with Phase 1)

- Issue #234 fix, regression test first (TDD).
- Docs sync: README version line, "what works" table, work-in-progress table brought to
  0.2.69 truth; product docs updated where shipped behaviour drifted.
- PRs #254/#253: review, unblock or close.

### Merge order

The external PRs #253/#254 go first: reviewed, then the owner merges or closes them
before any group rebases — both touch the same `telegram.rs`/`store.rs`/`draw.rs` files
the groups need. After that: `docs-sync` and #234 (small, file-disjoint). Then G1, G2a,
G2b, G3.

Merging runs interleaved: the owner merges a ready pull request while later groups are
still building; each later branch rebases onto the merged `main` right before it is
marked ready. Agents may start lab runs themselves on the hosts that are up; the blocked
macOS/iOS hosts follow when they are fixed.

## Approaches considered

- **A — one wave 4 branch, serial** (the #155/#232 pattern): fewest conflicts, one big
  review, no parallelism. Rejected: the batch is smaller than wave 3 and the owner asked
  for parallel throughput.
- **B — eight topic PRs fully parallel**: maximum nominal throughput, rejected: shared
  files make the agents fight; eight stacked reviews; conflicts surface at merge time when
  they are most expensive.
- **C — this design**: parallelism where state is disjoint (recon, cross-cutting, groups
  with separated file sets), serial where it is not (merge order). Chosen.

## Constraints

- `main` is squash-only and every change needs the owner's review (CODEOWNERS); agents
  never push to `main`.
- The shared checkout stays on `main` and untouched by agents;
  all agent work happens in worktrees.
- Nothing tags or releases unless the owner asks (release-captain rules).
- The lab records e2e proof per platform; agents do not paste their own screenshots.

## Rulings from the builder check

- **PARTIAL topics are extracted, not rebased.** The remainder is ported onto a fresh
  branch off current `origin/main`, taking hunks from the draft; the old draft branch is
  never rebased wholesale (it drags 98+ commits of drift and vendor churn).
- **Checkpoint before Phase 1.** Phase 0 ends with the backlog table shown to the owner;
  build dispatch waits for his go. "Superseded — the wave shipped an evolved form" is a
  legal verdict and drops a topic without a PR, as with ptt/voice.
- **Nothing is deleted before the table is confirmed**, and every branch is archived to a
  `git bundle` under `_archive/branches/` first. Local-only branches are otherwise
  irrecoverable.
- **G1 is GUI-only; `telegram/sync.rs` and the rest of the adapter live in G2.** The
  serial merge order handles the residual coupling.
- **External PRs #253/#254**: agents review and recommend; only the owner merges or
  closes an external contributor's pull request.
- **"Ready" means `pr-gate` green**, and agents drive the lab themselves on the hosts
  that are up (`e2e/gate.json` enforces `template` + `scenario` today, `release_proof`
  is not set to enforce, and the macOS/iOS lab hosts are recorded blocked). Those two
  platforms follow when their hosts are fixed.

## What is left to the builder

- Per-topic verdicts and the final backlog table come out of Phase 0 and refine the group
  assignments above; a topic that recon proves shipped drops out without a PR.
- Each group's agent writes its own e2e scenario, resolves its rebase conflicts, runs
  `cargo test --workspace` and `cargo check -p tinyclient --features gui`, and opens the
  pull request from the template.
- G2's split into sub-waves may shift one topic either way based on the recon file lists.
