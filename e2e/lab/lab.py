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
    """An adapter's run, then the lab's own check of what it brought back. Never raises."""
    adapter = load_adapter(platform)
    where = os.path.join(out, platform, label)
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
    if branch and not branch["passed"]:
        why.append("it fails on the branch")
    if base is not None and base["passed"]:
        why.append("it passes on the base, so it proves nothing new")
    return ("failure", "; ".join(why)[:130]) if why else ("success", "%s: base failed, branch passed, both recorded" % scenario["id"] if base else "%s passed, recorded" % scenario["id"])


def platform_job(platform, scenarios, shas, src_tars, out, log):
    """Everything for one platform, one scenario after another. Returns (state, description, runs)."""
    runs, verdicts = [], []
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
    return state, "; ".join(bad or [d for _, d in verdicts])[:140] or "nothing to record", runs


def execute(plan, scenarios, shas, out, args, log, context_prefix, head_sha, repo):
    os.makedirs(out, exist_ok=True)
    src_tars = {"head": archive(shas["head"], os.path.join(out, "src-%s.tar" % shas["head"][:7]))}
    if shas["need_red"]:
        src_tars["base"] = archive(shas["base"], os.path.join(out, "src-%s.tar" % shas["base"][:7]))
    tag = args.tag
    url = "https://github.com/%s/releases/tag/%s" % (HOSTS["proof_repo"], tag)
    if not args.no_post:
        for p in plan:
            common.post_status(repo, head_sha, "%s/%s" % (context_prefix, p), "pending", "recording on %s" % p, url if not args.no_upload else None)
    groups = {}
    for p in plan:
        groups.setdefault(HOSTS["platforms"][p]["group"], []).append(p)

    def worker(group):
        done = {}
        for p in groups[group]:
            done[p] = platform_job(p, scenarios, shas, src_tars, out, log)
            state, desc, runs = done[p]
            log("%s: %s: %s" % (p, state, desc))
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
        upload(out, results, tag, repo, head_sha)
    return results, url


def upload(out, results, tag, repo, sha):
    proof_repo = HOSTS["proof_repo"]
    if common.gh("release", "view", tag, "-R", proof_repo, check=False).returncode != 0:
        common.gh("release", "create", tag, "-R", proof_repo, "--title", "%s %s" % (repo.split("/")[-1], tag),
                  "--notes", "Recordings of the lab's run of %s at %s. Made by e2e/lab/lab.py; every video has the SHA burned in." % (repo, sha))
    files = []
    for p, (_, _, runs) in results.items():
        files.append(os.path.join(out, p, "proof.json"))
        for r in runs:
            for key in ("recording", "steps"):
                if r.get(key) and os.path.isfile(r[key]):
                    files.append(r[key])
    # Names must be unique inside one release.
    staged = []
    for f in files:
        rel = os.path.relpath(f, out).replace(os.sep, "-")
        link = os.path.join(out, "upload-" + rel)
        if not os.path.exists(link):
            shutil.copyfile(f, link)
        staged.append(link)
    if staged:
        common.gh("release", "upload", tag, *staged, "-R", proof_repo, "--clobber")


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

    repo = repo_name()
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out = args.out or os.path.join(os.environ.get("LOCALAPPDATA") or os.path.expanduser("~/.cache"), "tiny-e2e", "lab", stamp)
    os.makedirs(out, exist_ok=True)
    log = common.log_to(os.path.join(out, "lab.log"))
    want = args.platforms.split(",") if args.platforms else None

    def scenario_of(sha, sid):
        # `try` is for building a scenario, so it reads the working tree; the rest read the commit.
        if args.mode == "try":
            sdir = os.path.join(out, "e2e", "scenarios", sid)
            shutil.copytree(os.path.join(ROOT, "e2e", "scenarios", sid), sdir, dirs_exist_ok=True)
        else:
            sdir = extract_scenario(sha, sid, out)
        return json.load(open(os.path.join(sdir, "scenario.json"), encoding="utf-8")), sdir

    if args.mode == "try":
        sha = git("rev-parse", args.sha or "origin/main")
        ensure(sha)
        scenarios = [scenario_of(sha, args.target)]
        plan = want or scenarios[0][0]["platforms"]
        args.no_post = args.no_upload = True
        args.tag = "try"
        results, _ = execute(plan, scenarios, {"head": sha, "base": None, "need_red": False}, out, args, log, "e2e-lab", sha, repo)
        for p, (state, desc, runs) in results.items():
            for r in runs:
                print("%s %s %s %s %s" % (p, state, r.get("recording") or "(no recording)", r["problems"], r.get("note", "")))
        return 0 if all(s == "success" for s, _, _ in results.values()) else 1

    if args.mode == "release":
        sha = git("rev-parse", args.sha or "origin/main")
        ensure(sha)
        ids = sorted(d for d in os.listdir(os.path.join(ROOT, "e2e", "scenarios")) if os.path.isfile(os.path.join(ROOT, "e2e", "scenarios", d, "scenario.json")))
        scenarios = [scenario_of(sha, i) for i in ids]
        scenarios = [(s, d) for s, d in scenarios if s.get("release")]
        plan = want or ALL
        args.tag = "main-" + sha[:7]
        shas = {"head": sha, "base": None, "need_red": False}
        context, post_sha = "e2e-lab-release", sha
        if not args.no_post:
            common.post_status(repo, sha, CFG["release_context"], "pending", "recording the release suite", None)
        results, url = execute(plan, scenarios, shas, out, args, log, context, post_sha, repo)
        ok = all(s == "success" for s, _, _ in results.values())
        if not args.no_post:
            common.post_status(repo, sha, CFG["release_context"], "success" if ok else "failure",
                               "release suite recorded on %s" % ", ".join(plan) if ok else "; ".join("%s: %s" % (p, d) for p, (s, d, _) in results.items() if s != "success"),
                               url if not args.no_upload else None)
        return 0 if ok else 1

    number = args.target
    info = json.loads(common.gh("pr", "view", number, "-R", repo, "--json", "headRefOid,baseRefName,body,files").stdout)
    head = info["headRefOid"]
    ensure(head, "pull/%s/head" % number)
    git("fetch", "-q", "origin", info["baseRefName"])
    base = git("merge-base", "origin/" + info["baseRefName"], head)
    body, files, statuses, _ = pr_gate.pr_inputs(repo, number, CFG)
    findings, plan, _, _, kind = pr_gate.evaluate(CFG, body, files, lambda sid: True, statuses)
    parsed = pr_gate.parse_body(body, CFG)
    plan = [p for p in plan if not want or p in want]
    args.tag = "pr-%s-%s" % (number, head[:7])
    url = "https://github.com/%s/releases/tag/%s" % (HOSTS["proof_repo"], args.tag)
    if kind in CFG["proof_types"]:
        ids = parsed["scenarios"]
    elif kind in CFG["regression_types"]:
        ids = ["smoke"]
    else:
        ids = []
    if not ids or not plan:
        log("nothing to record for a %s pull request" % kind)
        if not args.no_post:
            common.post_status(repo, head, CFG["proof_context"], "success", "nothing to record (%s)" % (kind or "no type"))
        return 0
    scenarios = [scenario_of(head, i) for i in ids]
    shas = {"head": head, "base": base, "need_red": kind in CFG["proof_types"]}
    if not args.no_post:
        common.post_status(repo, head, CFG["proof_context"], "pending", "recording on %s" % ", ".join(plan), None)
    results, url = execute(plan, scenarios, shas, out, args, log, "e2e-lab", head, repo)
    ok = all(s == "success" for s, _, _ in results.values())
    if not args.no_post:
        for p, (state, desc, _) in results.items():
            common.post_status(repo, head, "e2e-lab/" + p, state, desc, url if not args.no_upload else None)
        common.post_status(repo, head, CFG["proof_context"], "success" if ok else "failure",
                           ("recorded on %s: red then green" % ", ".join(plan)) if ok else "; ".join("%s: %s" % (p, d) for p, (s, d, _) in results.items() if s != "success"),
                           url if not args.no_upload else None)
    print(json.dumps({p: {"state": s, "description": d} for p, (s, d, _) in results.items()}, indent=2))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
