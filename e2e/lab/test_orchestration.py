"""The lab's judgement of red-then-green, with fake runs: no hosts, no ffmpeg."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import lab  # noqa: E402

SCENARIO = {"id": "mute-room", "platforms": ["linux", "ios"], "not_applicable": {"windows": "the menu is phone-only"}}


def run(label, passed, problems=()):
    return {"label": label, "passed": passed, "problems": list(problems), "app_sha": "x", "recording": "" if problems else "%s.mp4" % label}


class Judge(unittest.TestCase):
    def test_base_fails_then_branch_passes_is_success(self):
        self.assertEqual(lab.judge(SCENARIO, "linux", run("base", False), run("branch", True))[0], "success")

    def test_passing_on_the_base_proves_nothing_new(self):
        state, why = lab.judge(SCENARIO, "linux", run("base", True), run("branch", True))
        self.assertEqual(state, "failure")
        self.assertIn("proves nothing new", why)

    def test_failing_on_the_branch_fails(self):
        state, why = lab.judge(SCENARIO, "linux", run("base", False), run("branch", False))
        self.assertEqual((state, "it fails on the branch" in why), ("failure", True))

    def test_a_bad_recording_fails_even_when_the_result_is_right(self):
        state, why = lab.judge(SCENARIO, "linux", run("base", False), run("branch", True, ["the picture is black for 80% of the run"]))
        self.assertEqual(state, "failure")
        self.assertIn("black", why)

    def test_a_missing_base_recording_fails(self):
        state, _ = lab.judge(SCENARIO, "linux", run("base", False, ["there is no recording"]), run("branch", True))
        self.assertEqual(state, "failure")

    def test_regression_runs_have_no_base(self):
        self.assertEqual(lab.judge(SCENARIO, "linux", None, run("branch", True))[0], "success")


class Platforms(unittest.TestCase):
    def job(self, platform, need_red=True, calls=None):
        def fake(platform_, label, *rest):
            (calls if calls is not None else []).append(label)
            return run(label, label == "branch")
        real, lab.one_run = lab.one_run, fake
        try:
            return lab.platform_job(platform, [(SCENARIO, "dir")], {"head": "h", "base": "b", "need_red": need_red},
                                    {"head": "t", "base": "t"}, "out", print)
        finally:
            lab.one_run = real

    def test_a_listed_platform_runs_base_then_branch(self):
        calls = []
        state, _, runs = self.job("linux", calls=calls)
        self.assertEqual((state, calls, len(runs)), ("success", ["base", "branch"], 2))

    def test_a_platform_the_scenario_does_not_cover_fails(self):
        state, why, runs = self.job("macos")
        self.assertEqual((state, runs), ("failure", []))
        self.assertIn("does not cover macos", why)

    def test_a_platform_with_a_stated_reason_is_not_applicable(self):
        state, why, runs = self.job("windows")
        self.assertEqual((state, runs), ("success", []))
        self.assertIn("the menu is phone-only", why)

    def test_a_regression_run_skips_the_base(self):
        calls = []
        self.job("linux", need_red=False, calls=calls)
        self.assertEqual(calls, ["branch"])


CFG = lab.CFG
FIVE = sorted(CFG["labs"])


def ok(p, runs=1):
    return ("success", "%s ok" % p, [{"label": "branch", "recording": "r-%s.mp4" % p, "steps": ""} for _ in range(runs)])


def bad(p):
    return ("failure", "%s broke" % p, [])


class Aggregate(unittest.TestCase):
    """The one status that certifies a change. A subset of the plan never does."""

    def test_every_required_platform_green_certifies(self):
        state, _ = lab.aggregate(FIVE, {p: ok(p) for p in FIVE})
        self.assertEqual(state, "success")

    def test_a_subset_of_the_plan_posts_nothing(self):
        self.assertIsNone(lab.aggregate(FIVE, {"linux": ok("linux")}))

    def test_one_red_platform_fails_the_whole(self):
        results = {p: ok(p) for p in FIVE}
        results["ios"] = bad("ios")
        state, why = lab.aggregate(FIVE, results)
        self.assertEqual(state, "failure")
        self.assertIn("ios", why)

    def test_no_results_never_certifies(self):
        self.assertIsNone(lab.aggregate(FIVE, {}))
        self.assertIsNone(lab.aggregate([], {}))

    def test_nothing_recorded_anywhere_does_not_certify(self):
        results = {p: ("success", "n/a", []) for p in FIVE}
        self.assertEqual(lab.aggregate(FIVE, results)[0], "failure")


class Published(unittest.TestCase):
    def setUp(self):
        import tempfile
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.out = self.dir.name
        os.makedirs(os.path.join(self.out, "linux"))
        self.rec = os.path.join(self.out, "linux", "linux-branch-smoke.mp4")
        for f in (self.rec, os.path.join(self.out, "linux", "proof.json")):
            open(f, "w").close()
        self.runs = [{"label": "branch", "recording": self.rec, "steps": ""}]

    def names(self, *files):
        return {lab.asset_name(self.out, f) for f in files}

    def test_green_with_all_evidence_published_stays_green(self):
        published = self.names(self.rec, os.path.join(self.out, "linux", "proof.json"))
        self.assertEqual(lab.verified(self.out, "linux", ("success", "ok", self.runs), published)[0], "success")

    def test_green_with_a_recording_missing_from_the_release_fails(self):
        published = self.names(os.path.join(self.out, "linux", "proof.json"))
        state, why, _ = lab.verified(self.out, "linux", ("success", "ok", self.runs), published)
        self.assertEqual(state, "failure")
        self.assertIn("not published", why)

    def test_green_with_nothing_published_fails(self):
        self.assertEqual(lab.verified(self.out, "linux", ("success", "ok", self.runs), set())[0], "failure")

    def test_a_red_result_is_left_alone(self):
        self.assertEqual(lab.verified(self.out, "linux", ("failure", "x", self.runs), set())[0], "failure")

    def test_a_platform_that_was_not_applicable_needs_no_recording(self):
        self.assertEqual(lab.verified(self.out, "linux", ("success", "n/a", []), set())[0], "success")


class BeforeRecording(unittest.TestCase):
    """What a pull request's type and plan decide before any host is touched."""

    def test_docs_with_nothing_to_record_succeeds(self):
        self.assertEqual(lab.pre_verdict(CFG, "docs", ["x"], [])[0], "success")

    def test_docs_that_changes_code_fails(self):
        self.assertEqual(lab.pre_verdict(CFG, "docs", [], FIVE)[0], "failure")

    def test_no_type_fails_even_with_nothing_to_record(self):
        self.assertEqual(lab.pre_verdict(CFG, None, [], [])[0], "failure")
        self.assertEqual(lab.pre_verdict(CFG, None, [], FIVE)[0], "failure")

    def test_feature_without_a_scenario_fails(self):
        self.assertEqual(lab.pre_verdict(CFG, "feature", [], FIVE)[0], "failure")
        self.assertEqual(lab.pre_verdict(CFG, "fix", [], FIVE)[0], "failure")

    def test_feature_with_a_scenario_goes_on_to_record(self):
        self.assertIsNone(lab.pre_verdict(CFG, "feature", ["mute-room"], FIVE))

    def test_refactor_goes_on_to_record(self):
        self.assertIsNone(lab.pre_verdict(CFG, "refactor", ["smoke"], FIVE))


class Flags(unittest.TestCase):
    def test_posting_without_uploading_is_refused(self):
        self.assertIn("--no-post", lab.flag_problem(no_post=False, no_upload=True))

    def test_local_runs_are_allowed(self):
        self.assertIsNone(lab.flag_problem(no_post=True, no_upload=True))
        self.assertIsNone(lab.flag_problem(no_post=True, no_upload=False))
        self.assertIsNone(lab.flag_problem(no_post=False, no_upload=False))


# ---- main() with every outside effect intercepted ------------------------------------------------

import contextlib  # noqa: E402
import io  # noqa: E402
import json  # noqa: E402
import tempfile  # noqa: E402
from unittest import mock  # noqa: E402

SHA = "a" * 40


class Harness:
    """Runs lab.main() with git, hosts, uploads and GitHub replaced. `posts` is every status it asked for."""

    def __init__(self, scenarios, ids_at=None, red=(), published="all", body="", files=("crates/chatd/src/lib.rs",)):
        self.scenarios = scenarios          # id -> scenario.json dict, as the TARGET commit has them
        self.ids_at = list(scenarios) if ids_at is None else ids_at
        self.red = set(red)                 # platforms whose branch run fails
        self.published = published          # "all", or "none" to lose the upload
        self.body, self.files = body, list(files)
        self.posts, self.runs = [], []
        self.base_passes = False

    def go(self, *argv):
        out = tempfile.mkdtemp()
        self.out = out

        def extract(sha, sid, dest):
            if sid not in self.scenarios:
                raise OSError("no such scenario on the commit: " + sid)
            d = os.path.join(dest, "e2e", "scenarios", sid)
            os.makedirs(d, exist_ok=True)
            with open(os.path.join(d, "scenario.json"), "w") as f:
                json.dump(self.scenarios[sid], f)
            return d

        def one_run(platform, label, app_sha, scenario_sha, scenario, sdir, src, outdir, log):
            self.runs.append((platform, label, scenario["id"]))
            if label == "base":
                green = self.base_passes
            else:
                green = platform not in self.red
            where = os.path.join(outdir, platform, label, scenario["id"])
            return {"label": label, "passed": green, "problems": [], "app_sha": app_sha,
                    "recording": os.path.join(where, "r.mp4"), "steps": os.path.join(where, "s.jsonl")}

        def publish(outdir, results, tag, repo, sha):
            if self.published == "none":
                return set()
            return {lab.asset_name(outdir, f) for p, (_, _, runs) in results.items() for f in lab.evidence_files(outdir, p, runs)}

        def tar(sha, dest, paths=()):
            open(dest, "w").close()
            return dest

        info = json.dumps({"headRefOid": SHA, "baseRefName": "main", "body": self.body, "files": []})
        with contextlib.ExitStack() as stack:
            stack.enter_context(mock.patch.object(sys, "argv", ["lab.py", *argv, "--out", out]))
            for name, value in [("repo_name", lambda: "TinyComms/tinyclient"), ("git", lambda *a, **k: SHA), ("ensure", lambda *a, **k: None),
                                ("archive", tar), ("extract_scenario", extract), ("scenario_ids_at", lambda sha: self.ids_at),
                                ("one_run", one_run), ("publish", publish)]:
                stack.enter_context(mock.patch.object(lab, name, value))
            stack.enter_context(mock.patch.object(lab.common, "post_status", lambda repo, sha, ctx, state, desc, url=None: self.posts.append((ctx, state, url))))
            stack.enter_context(mock.patch.object(lab.common, "gh", lambda *a, **k: mock.Mock(stdout=info, returncode=0)))
            stack.enter_context(mock.patch.object(lab.pr_gate, "pr_inputs", lambda repo, number, cfg: (self.body, self.files, [], SHA)))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            self.code = lab.main()
        return self

    def posted(self, context):
        return [(s, u) for c, s, u in self.posts if c == context]


def scenario(sid, release=True):
    return {"id": sid, "platforms": list(lab.ALL), "release": release}


def pr_body(kind, ids=()):
    types = "\n".join("- [%s] %s" % ("x" if t == kind else " ", t) for t in lab.CFG["types"])
    proof = "\n".join("Scenario: e2e/scenarios/" + i for i in ids)
    return "## What changed and why\n\nA member can mute a room from the sidebar menu.\n\n## Type\n\n%s\n\n## Proof\n\n%s\n" % (types, proof)


class ReleaseCertification(unittest.TestCase):
    def test_a_full_green_release_certifies(self):
        h = Harness({"smoke": scenario("smoke")}).go("release")
        self.assertEqual((h.code, h.posted("e2e-release")[-1][0]), (0, "success"))
        self.assertEqual(sorted(p for p, _, _ in h.runs), sorted(lab.ALL))

    def test_release_on_one_platform_never_certifies_the_release(self):
        h = Harness({"smoke": scenario("smoke")}).go("release", "--platforms", "linux")
        self.assertEqual(h.posted("e2e-release"), [])
        self.assertEqual([p for p, _, _ in h.runs], ["linux"])
        self.assertEqual(h.posted("e2e-lab-release/linux")[-1][0], "success")  # what it saw, for that platform

    def test_release_without_upload_is_refused_before_anything_runs(self):
        h = Harness({"smoke": scenario("smoke")}).go("release", "--platforms", "linux", "--no-upload")
        self.assertEqual((h.code, h.posts, h.runs), (2, [], []))

    def test_release_without_upload_for_all_platforms_is_refused_too(self):
        h = Harness({"smoke": scenario("smoke")}).go("release", "--no-upload")
        self.assertEqual((h.code, h.posts, h.runs), (2, [], []))

    def test_local_release_run_posts_nothing_and_may_skip_the_upload(self):
        h = Harness({"smoke": scenario("smoke")}).go("release", "--no-post", "--no-upload")
        self.assertEqual((h.posts, h.code), ([], 0))

    def test_a_lost_upload_turns_green_platforms_red_and_the_release_fails(self):
        h = Harness({"smoke": scenario("smoke")}, published="none").go("release")
        self.assertEqual((h.code, h.posted("e2e-release")[-1][0]), (1, "failure"))
        self.assertEqual({s for s, _ in h.posted("e2e-lab-release/linux")}, {"pending", "failure"})

    def test_per_platform_pending_statuses_are_resolved(self):
        h = Harness({"smoke": scenario("smoke")}).go("release")
        for p in lab.ALL:
            self.assertEqual(h.posted("e2e-lab-release/%s" % p)[-1][0], "success", p)

    def test_an_unknown_platform_is_refused(self):
        h = Harness({"smoke": scenario("smoke")}).go("release", "--platforms", "linux,beos")
        self.assertEqual((h.code, h.posts, h.runs), (2, [], []))

    def test_one_red_platform_fails_the_release(self):
        h = Harness({"smoke": scenario("smoke")}, red=["ios"]).go("release")
        self.assertEqual((h.code, h.posted("e2e-release")[-1][0]), (1, "failure"))


class ReleaseSuiteIsTheTargetCommits(unittest.TestCase):
    def test_a_scenario_added_in_the_target_is_in_the_suite(self):
        h = Harness({"smoke": scenario("smoke"), "added": scenario("added")}, ids_at=["added", "smoke"]).go("release", "--no-post", "--no-upload")
        self.assertEqual({sid for _, _, sid in h.runs}, {"smoke", "added"})

    def test_a_scenario_only_this_checkout_has_is_not(self):
        # smoke exists in the working tree here; the target commit does not list it
        h = Harness({"added": scenario("added")}, ids_at=["added"]).go("release", "--no-post", "--no-upload")
        self.assertEqual({sid for _, _, sid in h.runs}, {"added"})

    def test_a_scenario_not_marked_release_is_left_out(self):
        h = Harness({"smoke": scenario("smoke"), "mute": scenario("mute", release=False)}).go("release", "--no-post", "--no-upload")
        self.assertEqual({sid for _, _, sid in h.runs}, {"smoke"})

    def test_an_empty_suite_is_a_failure_not_a_pass(self):
        h = Harness({"mute": scenario("mute", release=False)}).go("release")
        self.assertEqual((h.code, h.runs), (1, []))
        self.assertEqual(h.posted("e2e-release"), [("failure", None)])

    def test_a_commit_with_no_scenarios_at_all_fails(self):
        h = Harness({}, ids_at=[]).go("release")
        self.assertEqual((h.code, h.posted("e2e-release")), (1, [("failure", None)]))

    def test_a_narrowed_empty_suite_posts_no_aggregate_but_still_fails(self):
        h = Harness({}, ids_at=[]).go("release", "--platforms", "linux")
        self.assertEqual((h.code, h.posted("e2e-release")), (1, []))

    def test_suite_selection_alone(self):
        suite = lab.release_suite(["a", "b"], lambda sid: ({"id": sid, "release": sid == "b"}, "/d/" + sid))
        self.assertEqual([s["id"] for s, _ in suite], ["b"])


class PullRequestCertification(unittest.TestCase):
    def fix(self, **kw):
        return Harness({"mute": scenario("mute", release=False)}, body=pr_body("fix", ["mute"]), **kw)

    def test_all_platforms_green_certifies(self):
        h = self.fix().go("pr", "170")
        self.assertEqual(h.posted("e2e-proof")[-1][0], "success")

    def test_a_subset_run_certifies_nothing(self):
        h = self.fix().go("pr", "170", "--platforms", "linux")
        self.assertEqual(h.posted("e2e-proof"), [])
        self.assertEqual({p for p, _, _ in h.runs}, {"linux"})

    def test_a_subset_outside_the_plan_is_refused(self):
        h = self.fix(files=["mobile/ios/Sources/App.swift"]).go("pr", "170", "--platforms", "linux")
        self.assertEqual((h.code, h.posts), (2, []))

    def test_pr_without_upload_is_refused(self):
        h = self.fix().go("pr", "170", "--no-upload")
        self.assertEqual((h.code, h.posts, h.runs), (2, [], []))

    def test_a_fix_runs_red_then_green_on_every_platform(self):
        h = self.fix().go("pr", "170", "--no-post", "--no-upload")
        for p in lab.ALL:
            self.assertEqual([label for q, label, _ in h.runs if q == p], ["base", "branch"], p)

    def test_a_fix_that_passes_on_the_base_does_not_certify(self):
        h = self.fix()
        h.base_passes = True
        h.go("pr", "170")
        self.assertEqual((h.code, h.posted("e2e-proof")[-1][0]), (1, "failure"))

    def test_a_fix_with_no_scenario_fails_and_records_nothing(self):
        h = Harness({}, body=pr_body("fix")).go("pr", "170")
        self.assertEqual((h.code, h.runs, h.posted("e2e-proof")), (1, [], [("failure", None)]))

    def test_docs_that_changes_code_fails(self):
        h = Harness({}, body=pr_body("docs")).go("pr", "170")
        self.assertEqual((h.code, h.posted("e2e-proof")), (1, [("failure", None)]))

    def test_unmapped_code_is_still_code_for_docs_and_scenario_rules(self):
        for kind in ("docs", "feature", "fix"):
            with self.subTest(kind=kind):
                h = Harness({}, body=pr_body(kind), files=["e2e/lab/lab.py"]).go("pr", "170")
                self.assertEqual((h.code, h.posted("e2e-proof")), (1, [("failure", None)]))

    def test_copied_manifest_ids_are_rejected_before_any_recording(self):
        manifests = {"one": scenario("smoke"), "two": scenario("smoke")}
        h = Harness(manifests, body=pr_body("fix", ["one", "two"])).go("pr", "170")
        self.assertEqual((h.code, h.runs, h.posted("e2e-proof")), (1, [], [("failure", None)]))

    def test_release_rejects_a_manifest_id_that_does_not_match_its_directory(self):
        h = Harness({"one": scenario("smoke")})
        try:
            h.go("release")
        except Exception as error:
            self.fail("invalid release scenario escaped instead of resolving proof: %s" % error)
        self.assertEqual((h.code, h.runs, h.posted("e2e-release")), (1, [], [("failure", None)]))

    def test_validation_exception_resolves_platform_and_release_statuses(self):
        h = Harness({"smoke": scenario("smoke")})
        with mock.patch.object(lab, "platform_job", side_effect=ValueError("truncated steps JSON")):
            try:
                h.go("release")
            except Exception as error:
                self.fail("validation exception left proof pending: %s" % error)
        self.assertEqual((h.code, h.posted("e2e-release")[-1][0]), (1, "failure"))
        for platform in lab.ALL:
            self.assertEqual(h.posted("e2e-lab-release/" + platform)[-1][0], "failure")

    def test_no_type_fails(self):
        h = Harness({}, body=pr_body(None)).go("pr", "170")
        self.assertEqual((h.code, h.posted("e2e-proof")), (1, [("failure", None)]))

    def test_docs_only_has_nothing_to_record_and_succeeds(self):
        h = Harness({}, body=pr_body("docs"), files=["docs/architecture.md"]).go("pr", "170")
        self.assertEqual((h.code, h.posted("e2e-proof")), (0, [("success", None)]))

    def test_a_scenario_missing_on_the_branch_fails(self):
        h = Harness({}, body=pr_body("fix", ["ghost"])).go("pr", "170")
        self.assertEqual((h.code, h.posted("e2e-proof")[-1][0]), (1, "failure"))

    def test_a_lost_upload_fails_the_pull_request(self):
        h = self.fix(published="none").go("pr", "170")
        self.assertEqual((h.code, h.posted("e2e-proof")[-1][0]), (1, "failure"))


class PerScenarioEvidence(unittest.TestCase):
    """Two scenarios on one platform must each keep the steps that go with their own recording."""

    def run_two(self):
        tmp = tempfile.mkdtemp()
        shared = os.path.join(tmp, "steps.jsonl")   # the adapter reuses one path, as a host would

        class Adapter:
            @staticmethod
            def run(ctx):
                with open(shared, "w") as f:
                    f.write(json.dumps({"t": 1, "step": "step of " + ctx.scenario["id"], "ok": True}) + "\n")
                video = os.path.join(ctx.out_dir, "raw.mp4")
                with open(video, "w") as f:
                    f.write("video of " + ctx.scenario["id"])
                return lab.common.Outcome(passed=True, recording=video, steps=shared, seconds=5)

        def caption(raw, out, header, steps, seconds):
            with open(raw) as src, open(out, "w") as dst:
                dst.write(src.read())
            return out

        with mock.patch.object(lab, "load_adapter", lambda platform: Adapter), \
                mock.patch.object(lab.common, "validate", lambda *a, **k: []), \
                mock.patch.object(lab.common, "caption", caption):
            return [lab.one_run("linux", "branch", SHA, SHA, {"id": sid}, tmp, "src.tar", tmp, print) for sid in ("one", "two")], tmp

    def test_each_scenario_keeps_its_own_steps(self):
        (first, second), _ = self.run_two()
        self.assertNotEqual(first["steps"], second["steps"])
        with open(first["steps"]) as a, open(second["steps"]) as b:
            self.assertIn("step of one", a.read())
            self.assertIn("step of two", b.read())

    def test_each_scenario_keeps_its_own_recording(self):
        (first, second), _ = self.run_two()
        self.assertNotEqual(first["recording"], second["recording"])
        with open(first["recording"]) as a:
            self.assertIn("video of one", a.read())

    def test_evidence_names_are_unique_in_a_release(self):
        (first, second), tmp = self.run_two()
        names = [lab.asset_name(tmp, f) for f in (first["steps"], second["steps"], first["recording"], second["recording"])]
        self.assertEqual(len(set(names)), 4)


class NoScenarios(unittest.TestCase):
    def test_malformed_driver_steps_are_a_failed_run(self):
        class Adapter:
            @staticmethod
            def run(ctx):
                return lab.common.Outcome(passed=True, steps="broken.jsonl", seconds=5)

        with tempfile.TemporaryDirectory() as out, \
                mock.patch.object(lab, "repo_name", return_value="TinyComms/tinyclient"), \
                mock.patch.object(lab, "load_adapter", return_value=Adapter), \
                mock.patch.object(lab.common, "read_steps", side_effect=ValueError("truncated steps JSON")):
            try:
                result = lab.one_run("linux", "branch", SHA, SHA, scenario("smoke"), out, "src.tar", out, lambda line: None)
            except Exception as error:
                self.fail("malformed driver evidence escaped the run: %s" % error)
        self.assertFalse(result["passed"])
        self.assertTrue(result["problems"])

    def test_a_platform_given_no_scenario_fails(self):
        state, why, runs = lab.platform_job("linux", [], {"head": "h", "base": "b", "need_red": True}, {}, "out", print)
        self.assertEqual((state, runs), ("failure", []))
        self.assertIn("no scenario", why)

    def test_a_missing_recording_is_not_a_success(self):
        norec = dict(run("branch", True), recording="")
        self.assertEqual(lab.judge(SCENARIO, "linux", None, norec)[0], "failure")


if __name__ == "__main__":
    unittest.main()
