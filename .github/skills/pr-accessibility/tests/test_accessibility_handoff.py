"""Markdown boundary and compiled-graph regression tests, not model evaluation."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import yaml

ROOT = Path(__file__).parents[4]
SPEC = importlib.util.spec_from_file_location(
    "handoff", ROOT / ".github/skills/pr-accessibility/scripts/accessibility_handoff.py"
)
handoff = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(handoff)
CHANGED = {"src/cascadia/TerminalApp/Test.xaml", "src/cascadia/TerminalApp/en-GB.resw"}


class MarkdownHandoffTests(unittest.TestCase):
    def test_existing_review_context_preserves_reply_disposition_and_source_identity(self):
        comments = [
            {"id": 2, "in_reply_to_id": 1, "body": "Fixed in exact-sha; no-results now announces.",
             "commit_id": "b" * 40, "path": "TabStrip.cpp", "html_url": "https://example.invalid/reply",
             "user": {"login": "maintainer"}},
            {"id": 1, "body": "Search gives no no-results announcement.",
             "commit_id": "a" * 40, "path": "TabStrip.cpp", "user": {"login": "reviewer"}},
        ]
        with patch.object(handoff.subprocess, "check_output",
                          return_value=json.dumps(comments).encode()) as fetch:
            context = handoff.prior_reviews("microsoft/intelligent-terminal", 1288)
        self.assertEqual("available", context["coverage"])
        self.assertEqual(1, context["comments"][0]["in_reply_to_id"])
        self.assertEqual(comments[0]["body"], context["comments"][0]["body"])
        self.assertEqual("b" * 40, context["comments"][0]["commit_id"])
        self.assertEqual("https://github.com/microsoft/intelligent-terminal/commit/" + "b" * 40,
                         context["comments"][0]["source_commit_url"])
        self.assertEqual(comments[0]["html_url"], context["comments"][0]["html_url"])
        self.assertIn("per_page=20&sort=updated&direction=desc", fetch.call_args.args[0][-1])
        self.assertNotIn("novel", context)  # evidence, never automatic adjudication

    def test_context_api_failure_and_truncation_cannot_mean_no_prior_findings(self):
        for error in (subprocess.TimeoutExpired("gh", 30), OSError("unavailable"),
                      ValueError("invalid JSON")):
            with self.subTest(error=type(error).__name__), patch.object(
                    handoff.subprocess, "check_output", side_effect=error):
                context = handoff.prior_reviews("microsoft/intelligent-terminal", 1213)
                self.assertEqual("unavailable", context["coverage"])
                self.assertIn("unknown", context["reason"])
                self.assertEqual([], context["comments"])
        for response in ({}, [None], [{"body": 1}], [{"user": None}]):
            with self.subTest(response=response), patch.object(
                    handoff.subprocess, "check_output", return_value=json.dumps(response).encode()):
                self.assertEqual("unavailable", handoff.prior_reviews("owner/repo", 1)["coverage"])
        with patch.object(handoff.subprocess, "check_output", return_value=b'[]'):
            self.assertEqual("available", handoff.prior_reviews("owner/repo", 1)["coverage"])
        with patch.object(handoff.subprocess, "check_output",
                          return_value=b'[{"commit_id": "not-an-immutable-sha"}]'):
            self.assertIsNone(handoff.prior_reviews("owner/repo", 1)["comments"][0]["source_commit_url"])
        with patch.object(handoff.subprocess, "check_output",
                          return_value=json.dumps([{}] * 21).encode()):
            context = handoff.prior_reviews("owner/repo", 1)
            self.assertEqual("limited", context["coverage"])
            self.assertEqual(20, len(context["comments"]))
        with patch.object(handoff.subprocess, "check_output",
                          return_value=json.dumps([{"body": "x" * 3000}]).encode()):
            context = handoff.prior_reviews("owner/repo", 1)
            self.assertEqual("limited", context["coverage"])
            self.assertEqual(2000, len(context["comments"][0]["body"]))
            self.assertTrue(context["comments"][0]["body_truncated"])
        for repository, number in (("bad/repo/path", 1), ("owner/repo", 0)):
            with self.assertRaises(ValueError):
                handoff.prior_reviews(repository, number)

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
        self.assertEqual({"agent", "native-selection"}, set(native["needs"]))
        self.assertIn("needs.native-selection.result == 'success'", native["if"])
        self.assertIn("needs.native-selection.outputs.native == 'true'", native["if"])
        self.assertNotIn("triage", native["if"])
        self.assertIn("head-sha: ${{ github.event.pull_request.head.sha }}",
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
            {"agent", "native-runtime", "native-selection", "safe_outputs", "triage", "triage-route"},
            set(summary["needs"]),
        )
        for key in ("deep-result", "native-result", "publication-result", "triage-result",
                    "native-selection-result", "route"):
            self.assertIn(key, summary["with"])

    def test_native_never_gates_repair_publication(self):
        dependencies = self.jobs["safe_outputs"]["needs"]
        self.assertNotIn("native-runtime", dependencies)
        self.assertNotIn("native-selection", dependencies)
        self.assertNotIn("final-summary", dependencies)

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

    def test_review_context_is_fetched_and_preserved_by_deep_and_final(self):
        step = next(s for s in self.jobs["agent"]["steps"]
                    if s.get("name") == "Read existing PR review context")
        self.assertIn("accessibility_handoff.py context", step["run"])
        self.assertIn("prior-reviews.json", step["run"])
        self.assertEqual("${{ github.token }}", step["env"]["GH_TOKEN"])
        deep = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        summary = (ROOT / ".github/workflows/ghaw-accessibility-summary.md").read_text(encoding="utf-8")
        self.assertIn("known/repeated", deep)
        self.assertIn("do not suppress a still-present real defect", deep)
        self.assertIn("latest 20 comments and first 2,000 characters", deep)
        self.assertIn("not evidence that the", deep)
        self.assertIn("known review dispositions and coverage limits", summary)
        self.assertIn("missing context means novelty", summary)

    def test_native_prerequisite_failure_retains_incomplete_receipt_before_upload(self):
        native = yaml.safe_load((ROOT / ".github/workflows/native-accessibility.yml").read_text())
        steps = native["jobs"]["native-smoke"]["steps"]
        record = next(s for s in steps if s.get("name") == "Record native discovery outcomes")
        self.assertEqual("always()", record["if"])
        names = [s.get("name") for s in steps]
        self.assertLess(names.index(record["name"]), names.index("Upload native accessibility evidence"))
        for stage, identifier in (("Build accessibility test host", "build"),
                                  ("Acquire pinned Axe.Windows CLI", "axe"),
                                  ("Install test host framework dependencies", "dependencies")):
            self.assertEqual(identifier, next(s for s in steps if s.get("name") == stage)["id"])
        defaults = {"BUILD_OUTCOME": "success", "AXE_OUTCOME": "success",
                    "DEPENDENCIES_OUTCOME": "success", "FRE_OUTCOME": "success",
                    "AGENTS_OUTCOME": "success", "FRE_SETTINGS_OUTCOME": "success"}
        cases = [({}, "scan-results-available")]
        cases.append(({"BUILD_OUTCOME": "failure", "AXE_OUTCOME": "skipped",
                       "DEPENDENCIES_OUTCOME": "skipped", "FRE_OUTCOME": "skipped",
                       "AGENTS_OUTCOME": "skipped", "FRE_SETTINGS_OUTCOME": "skipped"}, "incomplete"))
        for key in defaults:
            for outcome in ("failure", "skipped", "cancelled", ""):
                cases.append(({key: outcome}, "incomplete"))
        for overrides, expected in cases:
            with self.subTest(overrides=overrides), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result = subprocess.run(
                    ["pwsh", "-NoProfile", "-NonInteractive", "-Command", record["run"]],
                    capture_output=True, text=True, timeout=30,
                    env={**os.environ, **defaults, **overrides,
                         "ACCESSIBILITY_RESULTS": str(root), "SOURCE_SHA": "a" * 40,
                         "TRUSTED_SHA": "b" * 40, "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "2"})
                self.assertEqual(0, result.returncode, result.stderr)
                receipt = json.loads((root / "native-outcome.json").read_text(encoding="utf-8-sig"))
                self.assertEqual(expected, receipt["status"])
                for key, outcome in overrides.items():
                    stage = key.removesuffix("_OUTCOME").lower().replace("_", "-")
                    self.assertEqual(outcome, {**receipt["stages"], **receipt["scans"]}[stage])
                self.assertEqual(["fre", "agents", "fre-settings"], receipt["selected_surfaces"])
                self.assertIn("Caller-selected Axe.Windows", receipt["selection"])
                self.assertEqual("a" * 40, receipt["source_sha"])
                self.assertEqual("b" * 40, receipt["trusted_sha"])
                self.assertEqual("2", receipt["attempt"])
                self.assertNotEqual("PASS", receipt["status"])

    def test_domain_outputs_are_generated_and_not_publication_paths(self):
        deep = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertNotIn("/tmp/gh-aw/accessibility/", deep)
        allowed = deep.split("    allowed-files:", 1)[1].split("    protected-files:", 1)[0]
        self.assertNotIn("Generated Files", allowed)
        self.assertIn("never stage or commit them", deep)
        self.assertIn("Do not build or test before committing or pushing", deep)


if __name__ == "__main__":
    unittest.main()
