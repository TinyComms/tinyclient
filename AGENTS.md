# Agents in tinyclient

## Release captain

These rules are complete here. You do not need `tiny/AGENTS.md` to follow them.

### What the captain does

The captain decides the cut and then stops. It does not merge feature work, fix a failing build, or open a feature pull request. Feature work stays on a branch and merges through a normal pull request into `main`. `main` is squash-only.

An agent doing feature work does not bump the version, tag, or announce. It may tell Justin that a client cut is waiting. It cuts only when Justin asks to ship, cut, or release.

Nothing tags or releases with no agent in the thread: no scheduler, no GitHub Action, no cron. No release on a day when no one asks for one.

### The quality gate

The whole gate is in `docs/quality-gate.md`; the lab's contract is `e2e/README.md`. For a feature agent:

- Open the pull request from `.github/pull_request_template.md`, with one type checked.
- A `feature` or `fix` brings an `e2e/scenarios/<id>` in the same pull request. It drives the real app and fails on `main` without the change.
- Do not paste your own screenshots as proof. The lab records each platform and posts `e2e-proof`.
- Merging with `--admin` past a missing `e2e-proof` is not allowed once it is required.
- GLM reviews every pull request (`docs/pr-review.md`) and posts inline findings and the check `Tiny Agents / Code Review`. It is advice, not a gate. Fix a finding or say in the thread why it is wrong; do not resolve it silently.

The captain's check prints an `e2e:` line: the lab's recorded release suite on the exact `origin/main` SHA. When `e2e/gate.json` has `"release_proof": "enforce"`, a missing one is a `BLOCK`.

### The check

`packaging/release-cut.sh` prints the cut, the next version, and every reason it may not ship today. It changes nothing unless it is passed `--apply`. Run it before every release and paste its output into the thread.

- Exit 0: ready. Exit 1: blocked, each reason is a `BLOCK` line. Exit 2: it could not work out the cut.
- `--apply --summary "<one line from the cut>"` makes the release commit and the tag and pushes both in one push. Pass it only when Justin asked to ship, cut, or release.
- `--check-release X.Y.Z` confirms the GitHub release has its files.

### The cut

The cut is every commit on `origin/main` since the previous `vX.Y.Z` tag. Something that should not ship is not merged. There is no list of merged commits to skip.

### When there is no release

No release when any of these hold:

- `packaging/release-freeze` covers today. One inclusive range per line, `YYYY-MM-DD YYYY-MM-DD`, in the local date of Justin's Windows machine (Zeppelin). A line that does not parse blocks too.
- A `v*` tag already points at a commit made today (local date). One release a local calendar day.
- `origin/main` has nothing since the last tag except a release commit.
- CI (`.github/workflows/ci.yml`) is not green on that exact `origin/main` SHA.
- The worktree is dirty, or the checkout is not `main` at `origin/main`.

Two overrides, each only on Justin's exact words. Every other check still runs.

- "second release today": pass `--second-release-today`. The release commit body records the phrase.
- "ship through the freeze": pass `--through-freeze`. The release commit body records the phrase.

The shared checkout at `tiny/tinyclient` is often on someone's branch with uncommitted work. Do not switch it, stash it, or clean it. Cut from a clean worktree on `main` instead.

### Making the release

1. The next version is a patch unless Justin names a minor (`--minor`) or a major (`--major`).
2. On an up-to-date `main`: bump the workspace `version` in `Cargo.toml`, run `cargo check` so `Cargo.lock` follows, commit `Release X.Y.Z: <one line from the cut>`, push that one commit to `main`. `release-cut.sh --apply` does exactly this.
3. The tag is `vX.Y.Z` and must match `Cargo.toml`. `packaging/ci-label.sh` fails the release run otherwise.
4. Watch `.github/workflows/release.yml` on the tag (`gh run watch <id> --exit-status`, in the background, never a sleep loop).
5. Confirm the GitHub release has `tinyclient-windows-x86_64-setup.exe` and `SHA256SUMS`: `packaging/release-cut.sh --check-release X.Y.Z`.
6. Discord: a published release posts to #releases from the release workflow when the webhook is set. A separate post is the `announce` input of `release.yml` (`workflow_dispatch`). Do not run it unless Justin asked.

### Done

When Justin asked to ship or update the client, done includes the installed app on Zeppelin reporting the new version and screenshots of that installed app. The `ship-it` skill is how that proof is done. A green GitHub release by itself is not done.

### tinyserver

tinyserver is not on the daily train. A client release does not wait for a server deploy, and a server deploy does not wait for a client release. The written compatibility rules are on the server side: tinyserver `docs/server.md` ("older clients keep working, and `GET /v1/capabilities` answers 404 on servers from before it"), the Compatibility table in tinyserver `docs/calls.md`, and tinyserver `docs/privacy/migration.md`.
