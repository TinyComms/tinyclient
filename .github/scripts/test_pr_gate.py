import os
import unittest

import importlib.util

HERE = os.path.dirname(__file__)
spec = importlib.util.spec_from_file_location("pr_gate", os.path.join(HERE, "pr-gate.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

CFG = gate.load_config()
TEMPLATE = open(os.path.join(HERE, "..", "pull_request_template.md"), encoding="utf-8").read()


def body(kind, what="Lets a member mute a room from the sidebar menu.", scenario=None, platforms=()):
    types = "\n".join("- [%s] %s" % ("x" if t == kind else " ", t) for t in CFG["types"])
    labs = "\n".join("- [%s] %s" % ("x" if t in platforms else " ", t) for t in CFG["labs"])
    proof = "Scenario: e2e/scenarios/%s" % scenario if scenario else "Scenario: e2e/scenarios/<id>"
    return "## What changed and why\n\n%s\n\n## Type\n\n%s\n\n## Platforms\n\n%s\n\n## Proof\n\n%s\n\n## Compatibility\n\nnone\n" % (
        what, types, labs, proof)


def run(text, files, scenarios=(), statuses=()):
    return gate.evaluate(CFG, text, files, lambda sid: sid in scenarios, list(statuses))


def rules(result):
    return [rule for rule, _ in result[0]]


GUI = "crates/tinyclient/src/gui/sidebar/menu.rs"


class Globs(unittest.TestCase):
    def test_double_star(self):
        self.assertTrue(gate.matches("crates/a/b/c.rs", ["crates/**"]))
        self.assertTrue(gate.matches("README.md", ["**/*.md"]))
        self.assertTrue(gate.matches("a/b/README.md", ["**/*.md"]))
        self.assertFalse(gate.matches("crates/a.rs", ["docs/**"]))
        self.assertFalse(gate.matches("mobile/android/a.kt", ["mobile/ios/**"]))


class Platforms(unittest.TestCase):
    def test_docs_reach_nothing(self):
        self.assertEqual(gate.platforms_for(["docs/a.md", "README.md", "site/x.html"], CFG), set())

    def test_shared_core_reaches_all_five(self):
        self.assertEqual(gate.platforms_for(["crates/chatd/src/lib.rs"], CFG), set(CFG["labs"]))

    def test_phone_ui_is_phones_only(self):
        self.assertEqual(gate.platforms_for(["crates/tinyclient/src/gui/phone/inbox.rs"], CFG), {"android", "ios"})

    def test_ios_shell_is_ios_only(self):
        self.assertEqual(gate.platforms_for(["mobile/ios/Sources/App.swift"], CFG), {"ios"})

    def test_windows_installer_is_windows_only(self):
        self.assertEqual(gate.platforms_for(["packaging/windows/tinyclient.iss"], CFG), {"windows"})


class Gate(unittest.TestCase):
    def test_unfilled_template_fails(self):
        findings = rules(run(TEMPLATE, [GUI]))
        self.assertEqual(findings.count("template"), 2)  # no description, no type

    def test_docs_only_pull_request_passes(self):
        result = run(body("docs"), ["docs/architecture.md"])
        self.assertEqual(result[0], [])
        self.assertEqual(result[1], [])

    def test_docs_type_with_code_fails(self):
        self.assertIn("template", rules(run(body("docs"), [GUI])))

    def test_two_types_fail(self):
        text = body("fix").replace("- [ ] feature", "- [x] feature")
        self.assertIn("template", rules(run(text, [GUI])))

    def test_feature_without_scenario_fails(self):
        self.assertEqual(rules(run(body("feature"), [GUI])), ["scenario"])

    def test_placeholder_scenario_does_not_count(self):
        self.assertEqual(rules(run(body("fix", scenario=None), [GUI])), ["scenario"])

    def test_scenario_that_does_not_exist_fails(self):
        self.assertEqual(rules(run(body("fix", scenario="mute-room"), [GUI, "e2e/scenarios/mute-room/scenario.json"])), ["scenario"])

    def test_scenario_not_in_the_diff_fails(self):
        self.assertEqual(rules(run(body("fix", scenario="mute-room"), [GUI], scenarios=["mute-room"])), ["scenario"])

    def test_feature_with_its_scenario_passes(self):
        files = [GUI, "e2e/scenarios/mute-room/scenario.json"]
        result = run(body("feature", scenario="mute-room"), files, scenarios=["mute-room"])
        self.assertEqual(result[0], [])
        self.assertEqual(result[1], sorted(CFG["labs"]))

    def test_refactor_needs_no_scenario_but_still_plans_the_lab(self):
        result = run(body("refactor"), ["crates/protocol/src/lib.rs"])
        self.assertEqual(result[0], [])
        self.assertEqual(result[1], sorted(CFG["labs"]))

    def test_declared_platform_adds_never_removes(self):
        result = run(body("refactor", platforms=["ios"]), ["packaging/windows/tinyclient.iss"])
        self.assertEqual(result[1], ["ios", "windows"])

    def test_lab_statuses_are_read_per_platform(self):
        statuses = [("e2e-lab/ios", "success"), ("e2e-lab/windows", "pending"), ("e2e-proof", "pending")]
        _, plan, lab, proof, _ = run(body("refactor", platforms=["ios"]), ["packaging/windows/tinyclient.iss"], statuses=statuses)
        self.assertEqual(lab, {"ios": "success", "windows": "pending"})
        self.assertEqual(proof, "pending")


class Actors(unittest.TestCase):
    def status(self, login, context="e2e-proof"):
        return {"context": context, "state": "success", "creator": {"login": login}}

    def test_unset_actors_trust_everyone(self):
        self.assertEqual(gate.trusted_state(self.status("anyone"), []), "success")

    def test_the_lab_account_counts(self):
        self.assertEqual(gate.trusted_state(self.status("tiny-lab"), ["tiny-lab"]), "success")

    def test_another_account_does_not(self):
        self.assertTrue(gate.trusted_state(self.status("someone-else"), ["tiny-lab"]).startswith("untrusted"))

    def test_other_contexts_are_left_alone(self):
        self.assertEqual(gate.trusted_state(self.status("someone-else", "ci"), ["tiny-lab"]), "success")


if __name__ == "__main__":
    unittest.main()
