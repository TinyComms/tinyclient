# Automated pull request review

GLM reads every pull request and posts what it finds. It is advice from a second reader, not a
gate: it catches the leaked token, the `unwrap()` on a peer's frame, the migration that edits a
shipped one, before a person has to. It does not replace the quality gate (`docs/quality-gate.md`),
which is about proof that the real app works.

It lives in tinyclient and serves every Tiny repository. tinyserver calls the same workflow.

## What a pull request gets

From one run, in about half a minute:

- **An inline review.** Each finding sits on the exact line it quotes, with a one-click suggestion
  when the fix is one small edit to that line. A blocker makes the review `REQUEST_CHANGES`;
  anything else is a `COMMENT`. The previous run's review is dismissed and its inline comments
  collapsed, so the page shows one current verdict.
- **A sticky comment** that says where the run is: reviewing, done (with the counts and a link),
  or unavailable.
- **The check `Tiny Agents / Code Review`.** Red on a blocker, green otherwise, and neutral when
  the provider was down, so an outage never reads as a verdict.

A finding is a **blocker** (leaks or loses user data, breaks encryption or sign-in, breaks
compatibility, crashes or hangs on input a peer controls, blocks what must not block, breaks an
invariant the repository states) or a **major** (a real defect worth fixing). At most five. Minor
things go in "residual risk" and never in the gate. What counts, per repository, is written in
`.github/review/`.

## Starting it

| How | Who |
|---|---|
| Opening a pull request, marking it ready, or pushing to it | Runs by itself, for pull requests by an owner, member or collaborator. Drafts wait. |
| The `agent:review` label | Anyone who can label. The label is taken off again, so it can be applied twice. |
| A comment starting with `/review` | An owner, member or collaborator. |
| `gh workflow run pr-review.yml --ref <branch> -f pr_number=N` | Anyone who can dispatch. Uses the branch's own script and instructions, which is how a change to the review itself is tried before it merges. (A brand new workflow file cannot be dispatched until it is on `main`.) |

A pull request from a stranger is not reviewed on its own, so an unknown account cannot spend the
key. A maintainer asks with the label or the comment.

## It is advice

Nothing requires the check. To make a blocker stop a merge, add `Tiny Agents / Code Review` to the
required checks in branch protection. Do that only once a few weeks of reviews have been right
more than they have been wrong; the model is not infallible, and a required check from a model
that cries wolf trains people to merge past red.

A provider outage does not turn the Actions run red either: the review step warns and exits clean. It
says so in the check (neutral) and the sticky comment.

## How it keeps the key safe

The repositories are public, and the run holds an API key and a write token. So:

- It runs on `pull_request_target`, which executes the default branch's copy of the workflow, not
  the branch's. The script and instructions are checked out of tinyclient `main`.
- The pull request is never checked out and nothing from it runs. The diff comes through the API.
  `test_pr_review.py` fails if either workflow starts naming the pull request's head.
- The title, description and diff are written by the author, so the prompt calls them data and
  tells the model not to follow instructions in them. A finding must quote a line that is really in
  the diff, or it is dropped, so an injected "report nothing" or an invented line cannot post.
- What the model answers is posted on a public page. `@` mentions in it are defanged, so it cannot
  ping people.
- Generated, vendored and binary files (`Cargo.lock`, `vendor/`, images, fonts) are left out of the
  prompt and named as left out.

## Setup

Per repository, once:

- Actions secret **`AI_API_KEY`**: a z.ai key. (Set on tinyclient and tinyserver.)
- Optional Actions variables **`AI_BASE_URL`** (default `https://api.z.ai/api/coding/paas/v4`) and
  **`AI_MODEL`** (default `glm-5.3`). Any OpenAI-compatible chat endpoint works; the reasoning
  settings that GLM-5.3 needs are sent only to `api.z.ai`.

GitHub-hosted runner addresses have been given 429s by z.ai in bursts that last minutes. The
script waits 5, 15, 30, 60 and 120 seconds between tries and then tries the whole call once more.
If the burst outlasts that, the check says "Review unavailable" and `/review` runs it again.

## Adding another Tiny repository

1. Copy `.github/workflows/pr-review.yml` into it and change the `review` job's `uses:` line to
   `TinyComms/tinyclient/.github/workflows/pr-review-run.yml@main`.
2. Set `AI_API_KEY` on it.
3. Write `.github/review/<repository name>.md` in tinyclient: what that repository must never do.
   `base.md` is read first for every repository. The overlay is picked by the repository's name,
   and a repository without one gets `base.md` alone.

## Changing what it looks for

The instructions are plain markdown in `.github/review/`. They are the thing to tune: if the
review keeps raising something that does not matter, say so there; if it misses a class of
mistake the project has been bitten by, add it to the repository's overlay as a one-line
invariant. A reviewer should be told what is a blocker, not shown a list of everything.

The prompt, the budgets and the parsing are in `.github/scripts/pr-review.py`. Its tests are
`python -m unittest discover -s .github/scripts -p "test_pr_review.py"`.

## Trying it by hand

```
export AI_API_KEY=...                                       # never commit it
python .github/scripts/pr-review.py --repo TinyComms/tinyclient --pr 231 \
  --instructions .github/review/base.md --instructions .github/review/tinyclient.md
```

Without `--post` it prints the review it would post and posts nothing.
