"""Markdown boundary and compiled-graph regression tests, not model evaluation."""
import importlib.util
from pathlib import Path
import unittest

import yaml

ROOT = Path(__file__).parents[4]
SPEC = importlib.util.spec_from_file_location(
    "handoff", ROOT / ".github/skills/pr-accessibility/scripts/accessibility_handoff.py"
)
handoff = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(handoff)
CHANGED = {"src/cascadia/TerminalApp/Test.xaml", "src/cascadia/TerminalApp/en-GB.resw"}


class MarkdownHandoffTests(unittest.TestCase):
    def test_three_routes_preserve_original_uncertainty(self):
        for route in ("Related", "None", "Uncertain"):
            original = f"# {route}\n\nCannot establish runtime impact from the diff.\n\n- `src/cascadia/TerminalApp/Test.xaml`\n"
            self.assertEqual(route, handoff.triage(original, CHANGED))
            self.assertIn("Cannot establish", original)

    def test_benign_comment_and_link_rewording_can_be_none_without_evidence_forms(self):
        self.assertEqual("None", handoff.triage(
            "# None\n\nOnly a XAML comment and en-GB documentation link text changed.\n",
            CHANGED,
        ))

    def test_uncertain_does_not_require_candidates_or_further_investigation(self):
        self.assertEqual("Uncertain", handoff.triage("# Uncertain\n\nImpact unclear.\n", CHANGED))

    def test_malformed_routes_fail_closed(self):
        for text in ("", "None", "# NONE\n\nReason", "# None\n",
                     "# Related\n\nReason", "# None\n\n- `unrelated/file`",
                     '# None\n\nReason\n\n- `$(touch bad)`',
                     "# None\n\nReason\n\n- `src/cascadia/TerminalApp/Test.xaml`\n- `src/cascadia/TerminalApp/Test.xaml`"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                handoff.triage(text, CHANGED)

    def test_native_requires_explicit_supported_selection(self):
        self.assertEqual("true", handoff.native("# Native checks: Axe.Windows\n\nRelevant settings UIA changes."))
        self.assertEqual("false", handoff.native("# Native checks: None\n\nSource-only change."))
        for text in ("", "# Uncertain\n\nMaybe native", "# Native checks: None",
                     "# Native checks: Axe.Windows\n\n", "# Native checks: Other\n\nReason"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                handoff.native(text)

    def test_oversized_handoffs_rejected(self):
        with self.assertRaises(ValueError):
            handoff.triage("# None\n\n" + "x" * 16384, CHANGED)
        with self.assertRaises(ValueError):
            handoff.native("# Native checks: None\n\n" + "x" * 16384)


class CompiledGraphTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.jobs = yaml.safe_load((ROOT / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(
            encoding="utf-8"))["jobs"]

    def test_none_skips_deep_related_and_uncertain_are_forwarded(self):
        condition = self.jobs["agent"]["if"]
        self.assertIn("needs.triage-route.result == 'success'", condition)
        self.assertIn("outputs.route == 'Related'", condition)
        self.assertIn("outputs.route == 'Uncertain'", condition)
        self.assertNotIn("outputs.route == 'None'", condition)
        self.assertIn("triage-route", self.jobs["agent"]["needs"])
        route = self.jobs["triage-route"]
        self.assertEqual("triage", route["needs"])
        self.assertIn("steps.route.outputs.route", route["outputs"]["route"])

    def test_native_requires_deep_selection_not_triage_uncertainty(self):
        selector = self.jobs["native-selection"]
        self.assertEqual("agent", selector["needs"])
        self.assertIn("steps.selection.outputs.native", selector["outputs"]["native"])
        native = self.jobs["native-runtime"]
        self.assertEqual({"agent", "native-selection", "safe_outputs"}, set(native["needs"]))
        self.assertIn("needs.native-selection.result == 'success'", native["if"])
        self.assertIn("needs.native-selection.outputs.native == 'true'", native["if"])
        self.assertNotIn("triage", native["if"])
        self.assertIn("head-sha: ${{ fromJSON(github.event.inputs.pull_request).head.sha }}",
                      (ROOT / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8"))

    def test_native_selection_survives_failed_deep_validation_but_not_skip_or_cancel(self):
        selector = self.jobs["native-selection"]
        self.assertEqual(
            "${{ !cancelled() && needs.agent.result != 'skipped' }}",
            selector["if"],
        )
        # Explicit status function overrides GitHub's implicit success() gate.
        # A failed report/repair validator may still leave a valid selection.
        for cancelled, result, expected in (
            (False, "success", True), (False, "failure", True),
            (False, "skipped", False), (True, "failure", False),
            (True, "cancelled", False),
        ):
            with self.subTest(cancelled=cancelled, result=result):
                self.assertEqual(expected, not cancelled and result != "skipped")
        self.assertIn("needs.agent.result == 'success'", self.jobs["safe_outputs"]["if"])
        upload = next(step for step in self.jobs["agent"]["steps"]
                      if step.get("name") == "Upload deep review evidence even when incomplete")
        self.assertIn("always()", upload["if"])

    def test_final_summary_runs_after_skips_and_failures(self):
        summary = self.jobs["final-summary"]
        self.assertIn("always()", summary["if"])
        self.assertEqual(
            {"admission", "agent", "native-runtime", "native-selection", "safe_outputs", "triage", "triage-route"},
            set(summary["needs"]),
        )
        for key in ("deep-result", "native-result", "publication-result", "triage-result",
                    "native-selection-result", "route"):
            self.assertIn(key, summary["with"])
        self.assertIn("needs.admission.result == 'success'", summary["if"])
        self.assertIn("needs.admission.outputs.applicable == 'true'", summary["if"])

    def test_all_costly_entrypoints_require_ci_admission(self):
        self.assertIn("admission", self.jobs["triage"]["needs"])
        self.assertIn("needs.admission.outputs.applicable == 'true'", self.jobs["triage"]["if"])
        self.assertIn("deep-admission", self.jobs["agent"]["needs"])
        self.assertIn("triage-route", self.jobs["deep-admission"]["needs"])
        for name in ("ghaw-accessibility-triage.lock.yml", "ghaw-accessibility-summary.lock.yml",
                     "native-accessibility.yml"):
            jobs = yaml.safe_load((ROOT / ".github/workflows" / name).read_text(encoding="utf-8"))["jobs"]
            costly = jobs.get("agent", jobs.get("native-smoke"))
            self.assertIn("admission", costly["needs"])
            self.assertTrue(any(step.get("uses") == "./.github/actions/accessibility-ci-admission"
                                for step in jobs["admission"]["steps"]))
        controller = yaml.safe_load((ROOT / ".github/workflows/ghaw-pr-accessibility-controller.yml").read_text())
        # PyYAML's YAML 1.1 parser represents the Actions `on` key as True.
        self.assertEqual(["PR CI"], controller[True]["workflow_run"]["workflows"])
        worker = yaml.safe_load((ROOT / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8"))
        self.assertNotIn("pull_request", worker[True])
        self.assertIn("workflow_dispatch", worker[True])

    def test_native_never_gates_repair_publication(self):
        dependencies = self.jobs["safe_outputs"]["needs"]
        self.assertNotIn("native-runtime", dependencies)
        self.assertNotIn("native-selection", dependencies)
        self.assertNotIn("final-summary", dependencies)

    def test_native_admits_confirmed_publication_but_scans_original_head(self):
        native = self.jobs["native-runtime"]
        # A status function retains discovery after failed/skipped publication.
        self.assertEqual(
            "${{ !cancelled() && needs.native-selection.result == 'success' && needs.native-selection.outputs.native == 'true' }}",
            native["if"],
        )
        self.assertEqual("${{ needs.safe_outputs.outputs.push_commit_sha }}",
                         native["with"]["published-sha"])
        self.assertEqual("${{ fromJSON(github.event.inputs.pull_request).head.sha }}",
                         native["with"]["head-sha"])
        workflow = yaml.safe_load((ROOT / ".github/workflows/native-accessibility.yml").read_text(
            encoding="utf-8"))
        declared = workflow[True]["workflow_call"]["inputs"]["published-sha"]
        self.assertFalse(declared["required"])
        self.assertEqual("", declared["default"])
        admission = next(step for step in workflow["jobs"]["admission"]["steps"]
                         if step.get("uses") == "./.github/actions/accessibility-ci-admission")
        self.assertEqual("${{ inputs.published-sha }}", admission["with"]["published-sha"])
        checkout = workflow["jobs"]["native-smoke"]["steps"][0]
        self.assertEqual("${{ inputs.head-sha }}", checkout["with"]["ref"])

    def test_reusable_workflow_permissions_fit_callers(self):
        levels = {"none": 0, "read": 1, "write": 2}
        for job_id in ("triage", "final-summary"):
            caller = self.jobs[job_id]
            child = yaml.safe_load((ROOT / caller["uses"].removeprefix("./")).read_text(encoding="utf-8"))
            for child_id, job in child["jobs"].items():
                requested = job.get("permissions", child.get("permissions", {}))
                for permission, level in requested.items():
                    with self.subTest(caller=job_id, child=child_id, permission=permission):
                        self.assertLessEqual(levels[level], levels[caller["permissions"].get(permission, "none")])
            # The artifact conclusion handler needs actions:write at the caller
            # ceiling, never on the model job. No reusable job may publish issues.
            self.assertNotIn("issues", child["jobs"]["agent"]["permissions"])
            self.assertNotIn("actions", child["jobs"]["agent"]["permissions"])
            self.assertNotIn("issues", caller["permissions"])

    def test_original_triage_is_available_to_deep_and_final(self):
        deep = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        summary = (ROOT / ".github/workflows/ghaw-accessibility-summary.md").read_text(encoding="utf-8")
        self.assertIn("Download original triage", deep)
        self.assertIn("inputs/accessibility-triage-*", summary)
        self.assertIn("including uncertainty", summary)
        self.assertIn("review incomplete", summary)
        self.assertIn("never \"No issues\"", summary)

    def test_domain_outputs_are_generated_and_not_publication_paths(self):
        deep = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertNotIn("/tmp/gh-aw/accessibility/", deep)
        allowed = deep.split("    allowed-files:", 1)[1].split("    protected-files:", 1)[0]
        self.assertNotIn("Generated Files", allowed)
        self.assertIn("never stage or commit them", deep)
        self.assertIn("Do not build or test before committing or pushing", deep)


if __name__ == "__main__":
    unittest.main()
