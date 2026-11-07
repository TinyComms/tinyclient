#!/usr/bin/env python3
"""The lab: drives the real app on each platform, records it, and posts what it saw.

  lab.py pr <number>          the pull request's scenarios on every platform its diff reaches:
                              base build (must fail) then branch build (must pass), recorded.
                              Posts e2e-lab/<platform> and e2e-proof on the head SHA.
  lab.py release [--sha SHA]  the release suite (scenario.json "release": true) on SHA (default
                              origin/main), all five platforms. Posts e2e-release on SHA.
  lab.py try <scenario> --sha SHA --platforms a,b
                              one scenario on the branch build only. Posts nothing; for building
                              a scenario or an adapter, and for looking at a recording.

Flags: --platforms a,b to narrow, --no-post / --no-upload to keep it local, --out DIR.
Needs `gh` (as the account in e2e/gate.json lab_actors), git, ffmpeg and ffprobe on this machine.
e2e/README.md is the contract an adapter in platforms/ follows; docs/quality-gate.md says why.
"""
import argparse
import concurrent.futures
import importlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)
import common  # noqa: E402
import coordinated  # noqa: E402

_spec = importlib.util.spec_from_file_location("pr_gate", os.path.join(ROOT, ".github", "scripts", "pr-gate.py"))
pr_gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pr_gate)

CFG = pr_gate.load_config()
HOSTS = json.load(open(os.path.join(HERE, "hosts.json"), encoding="utf-8"))
ALL = CFG["labs"]


def git(*args, check=True):
    return subprocess.run(["git", "-C", ROOT, *args], check=check, capture_output=True, text=True).stdout.strip()


def repo_name():
    url = git("remote", "get-url", "origin")
    return url.replace(".git", "").split("github.com")[-1].lstrip(":/")


def archive(sha, dest, paths=()):
    subprocess.run(["git", "-C", ROOT, "-c", "core.autocrlf=false", "archive", "--format=tar", "-o", dest, sha, *paths], check=True)
    return dest


def have(sha):
    return subprocess.run(["git", "-C", ROOT, "cat-file", "-e", sha + "^{commit}"], capture_output=True).returncode == 0


def ensure(sha, ref=None):
    if not have(sha):
        subprocess.run(["git", "-C", ROOT, "fetch", "-q", "origin", ref or sha], check=True,
                       env={**os.environ, "GIT_TERMINAL_PROMPT": "0"})
    if not have(sha):
        raise SystemExit("lab: cannot get %s" % sha)


def extract_scenario(sha, sid, out):
    tar = archive(sha, os.path.join(out, "scenario-%s.tar" % sid), ["e2e/scenarios/" + sid])
    with tarfile.open(tar) as t:
        t.extractall(out, filter="data")
    os.remove(tar)
    return os.path.join(out, "e2e", "scenarios", sid)


def load_adapter(platform):
    path = os.path.join(HERE, "platforms", platform + ".py")
    if not os.path.isfile(path):
        return None
    spec = importlib.util.spec_from_file_location("lab_platform_" + platform, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def one_run(platform, label, app_sha, scenario_sha, scenario, scenario_dir, src_tar, out, log):
    """Adapter, evidence validation and captioning all belong to the same failed-run boundary."""
    try:
        return _one_run(platform, label, app_sha, scenario_sha, scenario, scenario_dir, src_tar, out, log)
    except Exception as error:
        log("[%s %s] run failed: %s" % (platform, label, error))
        return {"passed": False, "problems": ["the run failed: " + str(error)], "label": label, "app_sha": app_sha}


def _one_run(platform, label, app_sha, scenario_sha, scenario, scenario_dir, src_tar, out, log):
    """An adapter's run, then the lab's own check of what it brought back. Never raises."""
    adapter = coordinated if scenario.get("execution") == "coordinated-ipc" else load_adapter(platform)
    # One folder per scenario: two scenarios on one platform must not overwrite each other's evidence.
    where = os.path.join(out, platform, label, scenario["id"])
    os.makedirs(where, exist_ok=True)
    if adapter is None:
        return {"passed": False, "problems": ["there is no lab for %s yet (e2e/lab/platforms/%s.py)" % (platform, platform)],
                "label": label, "app_sha": app_sha}
    ctx = common.Run(platform=platform, label=label, repo=repo_name(), app_sha=app_sha, scenario_sha=scenario_sha,
                     scenario=scenario, scenario_dir=scenario_dir, source_tar=src_tar, out_dir=where,
                     host=HOSTS["platforms"][platform], log=lambda line: log("[%s %s] %s" % (platform, label, line)))
    started = time.time()
    try:
        outcome = adapter.run(ctx)
    except Exception:  # an adapter bug is a failed platform, not a dead lab
        trace = traceback.format_exc()
        log("[%s %s] adapter crashed:\n%s" % (platform, label, trace))
        return {"passed": False, "problems": ["the adapter crashed: " + trace.strip().splitlines()[-1]], "label": label, "app_sha": app_sha}
    steps = common.read_steps(outcome.steps)
    if outcome.steps and os.path.isfile(outcome.steps):
        # Taken now, under a name of its own, before the adapter's next run can reuse the path.
        kept = os.path.join(where, "%s-%s-%s.steps.jsonl" % (platform, label, scenario["id"]))
        shutil.copyfile(outcome.steps, kept)
        outcome.steps = kept
    problems = common.validate(outcome.recording, outcome.seconds, steps, scenario,
                               expect_pass=bool(outcome.passed) or label == "branch", vfr=outcome.vfr)
    shown = ""
    if outcome.recording and not problems:
        header = "%s@%s %s %s %s" % (ctx.repo.split("/")[-1], scenario_sha[:7] if label == "branch" else app_sha[:7], platform, label, scenario["id"])
        shown = common.caption(outcome.recording, os.path.join(where, "%s-%s-%s.mp4" % (platform, label, scenario["id"])),
                               header, steps, outcome.seconds)
    result = {"label": label, "app_sha": app_sha, "passed": bool(outcome.passed), "problems": problems,
              "recording": shown, "raw": outcome.recording, "steps": outcome.steps, "logs": outcome.logs,
              "seconds": round(outcome.seconds, 1), "wall_seconds": round(time.time() - started, 1),
              "binary_sha256": outcome.binary_sha256, "note": outcome.note}
    if shown:
        result["sha256"] = common.sha256(shown)
    return result


def judge(scenario, platform, base, branch):
    """success only when the recordings are good and the scenario failed before and passes now."""
    why = []
    for r in (base, branch):
        if r:
            why += ["%s: %s" % (r["label"], p) for p in r["problems"]]
    for r in (base, branch):
        if r and not r.get("recording") and not r["problems"]:
            why.append("%s: there is no captioned recording" % r["label"])
    if branch and not branch["passed"]:
        why.append("it fails on the branch")
    if base is not None and base["passed"]:
        why.append("it passes on the base, so it proves nothing new")
    return ("failure", "; ".join(why)[:130]) if why else ("success", "%s: base failed, branch passed, both recorded" % scenario["id"] if base else "%s passed, recorded" % scenario["id"])


def platform_job(platform, scenarios, shas, src_tars, out, log):
    """Everything for one platform, one scenario after another. Returns (state, description, runs)."""
    runs, verdicts = [], []
    if not scenarios:
        return "failure", "no scenario was given to record on %s" % platform, []
    for scenario, sdir in scenarios:
        if platform not in scenario.get("platforms", []):
            reason = scenario.get("not_applicable", {}).get(platform)
            if reason:
                verdicts.append(("success", "%s: n/a on %s (%s)" % (scenario["id"], platform, reason)))
            else:
                verdicts.append(("failure", "%s does not cover %s" % (scenario["id"], platform)))
            continue
        base = None
        if shas["need_red"]:
            base = one_run(platform, "base", shas["base"], shas["head"], scenario, sdir, src_tars["base"], out, log)
        branch = one_run(platform, "branch", shas["head"], shas["head"], scenario, sdir, src_tars["head"], out, log)
        runs += [r for r in (base, branch) if r]
        verdicts.append(judge(scenario, platform, base, branch))
    bad = [d for s, d in verdicts if s != "success"]
    state = "failure" if bad else "success"
    return state, "; ".join(bad or [d for _, d in verdicts])[:140], runs


# ---- What may certify a change -------------------------------------------------------------
# These are pure so the invariants can be tested without a host or a status being posted.

def flag_problem(no_post, no_upload):
    """A status links published recordings. Posting without uploading would claim evidence nobody can see."""
    if no_upload and not no_post:
        return "--no-upload cannot post statuses, because a status must link recordings that are published. Add --no-post, or drop --no-upload."
    return None


def asset_name(out, path):
    """The name a file has inside the proof release. Unique per file: the path under out/ is in it."""
    return "upload-" + os.path.relpath(path, out).replace(os.sep, "-")


def evidence_files(out, platform, runs):
    files = [os.path.join(out, platform, "proof.json")]
    for r in runs:
        files += [r[key] for key in ("recording", "steps") if r.get(key)]
    return files


def verified(out, platform, result, published):
    """A green platform stays green only if every file that backs it is in the release."""
    state, desc, runs = result
    if state != "success" or not runs:
        return result
    lost = [os.path.basename(f) for f in evidence_files(out, platform, runs) if asset_name(out, f) not in published]
    if lost:
        return "failure", ("recordings not published: " + ", ".join(lost))[:140], runs
    return result


def aggregate(required, results):
    """The one status that certifies a change, from the full required plan.

    None when this run did not cover every required platform: a subset run (--platforms) is
    useful and may post what it saw per platform, but it never speaks for the whole plan."""
    if not required or set(required) - set(results):
        return None
    bad = ["%s: %s" % (p, results[p][1]) for p in required if results[p][0] != "success"]
    if bad:
        return "failure", "; ".join(bad)[:140]
    if not any(results[p][2] for p in required):
        return "failure", "nothing was recorded on any platform"
    return "success", ("recorded on %s" % ", ".join(required))[:140]


def pre_verdict(cfg, kind, ids, full_plan, code_changed=None):
    """What the type and the plan settle before any host is touched. None means: go and record."""
    if kind is None:
        return "failure", "the pull request has no single type"
    code_changed = bool(full_plan) if code_changed is None else code_changed
    if kind == "docs" and code_changed:
        return "failure", "it is typed docs but changes code"
    if kind in cfg["proof_types"] and code_changed and not ids:
        return "failure", "a %s needs an e2e scenario named under Proof" % kind
    if not full_plan:
        return "success", "no platform recording required (%s)" % kind
    return None


def scenario_ids_at(sha):
    """The scenarios that exist in commit `sha`, not in whatever this checkout has."""
    done = subprocess.run(["git", "-C", ROOT, "ls-tree", "--name-only", "%s:e2e/scenarios" % sha], capture_output=True, text=True)
    return sorted(done.stdout.split()) if done.returncode == 0 else []


def release_suite(ids, load):
    """The (scenario, dir) pairs marked release among `ids`. `load(id)` reads one from the target commit."""
    suite = []
    for sid in ids:
        scenario, sdir = load(sid)
        if scenario.get("release"):
            suite.append((scenario, sdir))
    return suite


def publish(out, results, tag, repo, sha):
    """Uploads the evidence and returns the names the release really holds afterwards."""
    proof_repo = HOSTS["proof_repo"]
    if common.gh("release", "view", tag, "-R", proof_repo, check=False).returncode != 0:
        common.gh("release", "create", tag, "-R", proof_repo, "--title", "%s %s" % (repo.split("/")[-1], tag),
                  "--notes", "Recordings of the lab's run of %s at %s. Made by e2e/lab/lab.py; every video has the SHA burned in." % (repo, sha))
    staged = []
    for p, (_, _, runs) in results.items():
        for f in evidence_files(out, p, runs):
            if os.path.isfile(f):
                link = os.path.join(out, asset_name(out, f))
                shutil.copyfile(f, link)
                staged.append(link)
    if staged:
        common.gh("release", "upload", tag, *staged, "-R", proof_repo, "--clobber")
    listed = common.gh("release", "view", tag, "-R", proof_repo, "--json", "assets", "--jq", ".assets[].name").stdout.split()
    return set(listed)


def execute(plan, scenarios, shas, out, args, log, context_prefix, head_sha, repo):
    """Infrastructure and worker failures resolve the requested statuses instead of leaving pending proof."""
    try:
        return _execute(plan, scenarios, shas, out, args, log, context_prefix, head_sha, repo)
    except Exception as error:
        description = ("lab execution failed: " + str(error))[:140]
        log(description)
        results = {platform: ("failure", description, []) for platform in plan}
        if not args.no_post:
            for platform in plan:
                common.post_status(repo, head_sha, "%s/%s" % (context_prefix, platform), "failure", description, None)
        return results, None


def _execute(plan, scenarios, shas, out, args, log, context_prefix, head_sha, repo):
    """Runs the plan. With posting on, it has already published the evidence by the time it posts
    a per-platform result, and a result whose evidence is missing is posted as a failure."""
    os.makedirs(out, exist_ok=True)
    src_tars = {"head": archive(shas["head"], os.path.join(out, "src-%s.tar" % shas["head"][:7]))}
    if shas["need_red"]:
        src_tars["base"] = archive(shas["base"], os.path.join(out, "src-%s.tar" % shas["base"][:7]))
    url = "https://github.com/%s/releases/tag/%s" % (HOSTS["proof_repo"], args.tag)
    posting = not args.no_post
    if posting:
        for p in plan:
            common.post_status(repo, head_sha, "%s/%s" % (context_prefix, p), "pending", "recording on %s" % p, None)
    groups = {}
    for p in plan:
        groups.setdefault(HOSTS["platforms"][p]["group"], []).append(p)

    def worker(group):
        done = {}
        for p in groups[group]:
            done[p] = platform_job(p, scenarios, shas, src_tars, out, log)
            log("%s: %s: %s" % (p, done[p][0], done[p][1]))
        return done

    results = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, len(groups))) as pool:
        for fut in concurrent.futures.as_completed([pool.submit(worker, g) for g in groups]):
            results.update(fut.result())
    for p, (state, desc, runs) in results.items():
        proof = {"repo": repo, "sha": head_sha, "base": shas.get("base"), "platform": p, "host": HOSTS["platforms"][p].get("group"),
                 "state": state, "description": desc, "runs": [{k: v for k, v in r.items() if k not in ("raw",)} for r in runs]}
        os.makedirs(os.path.join(out, p), exist_ok=True)
        with open(os.path.join(out, p, "proof.json"), "w", encoding="utf-8") as f:
            json.dump(proof, f, indent=2)
    if not args.no_upload:
        try:
            published = publish(out, results, args.tag, repo, head_sha)
        except Exception as e:  # an upload that failed is a platform without evidence, not a lab that hangs on pending
            log("upload failed: %s" % e)
            published = set()
        results = {p: verified(out, p, r, published) for p, r in results.items()}
    if posting:
        for p, (state, desc, _) in results.items():
            common.post_status(repo, head_sha, "%s/%s" % (context_prefix, p), state, desc, url)
    return results, url


def post_aggregate(args, repo, sha, context, full_plan, results, url, log):
    """Posts the certifying status, or says why it did not. Returns whether the run is green."""
    verdict = aggregate(full_plan, results)
    if verdict is None:
        log("this run covered %s of the required %s, so it certifies nothing; per-platform results only"
            % (",".join(sorted(results)) or "none", ",".join(full_plan)))
        return all(s == "success" for s, _, _ in results.values())
    state, desc = verdict
    if not args.no_post:
        common.post_status(repo, sha, context, state, desc, url)
    return state == "success"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("mode", choices=["pr", "release", "try"])
    ap.add_argument("target", nargs="?", help="pr: the number. try: the scenario id")
    ap.add_argument("--sha")
    ap.add_argument("--platforms")
    ap.add_argument("--no-post", action="store_true")
    ap.add_argument("--no-upload", action="store_true")
    ap.add_argument("--out")
    args = ap.parse_args()
    if args.mode == "try":
        args.no_post = args.no_upload = True
    problem = flag_problem(args.no_post, args.no_upload)
    if problem:
        print("lab: " + problem, file=sys.stderr)
        return 2

    repo = repo_name()
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out = args.out or os.path.join(os.environ.get("LOCALAPPDATA") or os.path.expanduser("~/.cache"), "tiny-e2e", "lab", stamp)
    os.makedirs(out, exist_ok=True)
    log = common.log_to(os.path.join(out, "lab.log"))
    want = args.platforms.split(",") if args.platforms else None
    unknown = [w for w in (want or []) if w not in ALL]
    if unknown:
        print("lab: unknown platform in --platforms: %s (known: %s)" % (",".join(unknown), ",".join(ALL)), file=sys.stderr)
        return 2

    def scenario_of(sha, sid):
        if not re.fullmatch(pr_gate.ID, sid):
            raise ValueError("invalid scenario directory ID: %s" % sid)
        # `try` is for building a scenario, so it reads the working tree; the rest read the commit.
        if args.mode == "try":
            sdir = os.path.join(out, "e2e", "scenarios", sid)
            shutil.copytree(os.path.join(ROOT, "e2e", "scenarios", sid), sdir, dirs_exist_ok=True)
        else:
            sdir = extract_scenario(sha, sid, out)
        with open(os.path.join(sdir, "scenario.json"), encoding="utf-8") as f:
            scenario = json.load(f)
        if scenario.get("id") != sid:
            raise ValueError("scenario ID must match directory %s" % sid)
        return scenario, sdir

    if args.mode == "try":
        sha = git("rev-parse", args.sha or "origin/main")
        ensure(sha)
        scenarios = [scenario_of(sha, args.target)]
        plan = want or scenarios[0][0]["platforms"]
        args.tag = "try"
        results, _ = execute(plan, scenarios, {"head": sha, "base": None, "need_red": False}, out, args, log, "e2e-lab", sha, repo)
        for p, (state, desc, runs) in results.items():
            for r in runs:
                print("%s %s %s %s %s" % (p, state, r.get("recording") or "(no recording)", r["problems"], r.get("note", "")))
        return 0 if results and all(s == "success" for s, _, _ in results.values()) else 1

    posting = not args.no_post

    if args.mode == "release":
        sha = git("rev-parse", args.sha or "origin/main")
        ensure(sha)
        full_plan = list(ALL)
        run_plan = [p for p in full_plan if not want or p in want]
        narrowed = run_plan != full_plan
        args.tag = "main-" + sha[:7]
        # The suite is what is in the commit being certified, not what this checkout happens to hold.
        try:
            scenarios = release_suite(scenario_ids_at(sha), lambda sid: scenario_of(sha, sid))
        except Exception as error:
            log("release scenarios could not be loaded: %s" % error)
            if posting and not narrowed:
                common.post_status(repo, sha, CFG["release_context"], "failure", "release scenarios could not be loaded", None)
            return 1
        if not scenarios:
            log("no scenario in %s is marked release, so there is no release suite to record" % sha[:7])
            if posting and not narrowed:
                common.post_status(repo, sha, CFG["release_context"], "failure", "no scenario is marked release", None)
            return 1
        if posting and not narrowed:
            common.post_status(repo, sha, CFG["release_context"], "pending", "recording the release suite", None)
        results, url = execute(run_plan, scenarios, {"head": sha, "base": None, "need_red": False}, out, args, log, "e2e-lab-release", sha, repo)
        return 0 if post_aggregate(args, repo, sha, CFG["release_context"], full_plan, results, url, log) else 1

    number = args.target
    info = json.loads(common.gh("pr", "view", number, "-R", repo, "--json", "headRefOid,baseRefName,body,files").stdout)
    head = info["headRefOid"]
    ensure(head, "pull/%s/head" % number)
    git("fetch", "-q", "origin", info["baseRefName"])
    base = git("merge-base", "origin/" + info["baseRefName"], head)
    body, files, statuses, _ = pr_gate.pr_inputs(repo, number, CFG)
    findings, full_plan, _, _, kind = pr_gate.evaluate(CFG, body, files, lambda sid: True, statuses)
    parsed = pr_gate.parse_body(body, CFG)
    ids = parsed["scenarios"] if kind in CFG["proof_types"] else ["smoke"] if kind in CFG["regression_types"] else []
    early = pre_verdict(CFG, kind, ids, full_plan, code_changed=bool(pr_gate.code_files(files, CFG)))
    if early:
        log("%s: %s" % early)
        if posting:
            common.post_status(repo, head, CFG["proof_context"], early[0], early[1], None)
        return 0 if early[0] == "success" else 1
    run_plan = [p for p in full_plan if not want or p in want]
    if not run_plan:
        print("lab: none of --platforms is in this pull request's plan (%s)" % ",".join(full_plan), file=sys.stderr)
        return 2
    narrowed = run_plan != full_plan
    args.tag = "pr-%s-%s" % (number, head[:7])
    try:
        scenarios = [scenario_of(head, i) for i in ids]
    except (OSError, subprocess.CalledProcessError, ValueError) as e:
        log("cannot read the scenario from the branch: %s" % e)
        if posting:
            common.post_status(repo, head, CFG["proof_context"], "failure", "a named scenario is not on the branch", None)
        return 1
    if posting and not narrowed:
        common.post_status(repo, head, CFG["proof_context"], "pending", "recording on %s" % ", ".join(full_plan), None)
    results, url = execute(run_plan, scenarios, {"head": head, "base": base, "need_red": kind in CFG["proof_types"]}, out, args, log, "e2e-lab", head, repo)
    green = post_aggregate(args, repo, head, CFG["proof_context"], full_plan, results, url, log)
    print(json.dumps({p: {"state": s, "description": d} for p, (s, d, _) in results.items()}, indent=2))
    return 0 if green else 1


if __name__ == "__main__":
    sys.exit(main())
