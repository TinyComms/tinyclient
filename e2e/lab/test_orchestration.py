"""The lab's judgement of red-then-green, with fake runs: no hosts, no ffmpeg."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import lab  # noqa: E402

SCENARIO = {"id": "mute-room", "platforms": ["linux", "ios"], "not_applicable": {"windows": "the menu is phone-only"}}


def run(label, passed, problems=()):
    return {"label": label, "passed": passed, "problems": list(problems), "app_sha": "x"}


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


if __name__ == "__main__":
    unittest.main()
