"""Compiled producer/consumer contracts for retained jobs on partial reruns.

These simulate artifact naming/lifecycle, not GitHub-hosted rerun execution.
"""
from fnmatch import fnmatchcase
from pathlib import Path
import unittest

import yaml

ROOT = Path(__file__).parents[4]


def workflow(name):
    return yaml.safe_load(
        (ROOT / f".github/workflows/{name}.lock.yml").read_text(encoding="utf-8"))


def expand(template, attempt):
    return template.replace("${{ github.run_id }}", "12345").replace(
        "${{ github.run_attempt }}", str(attempt))


def step_named(job, name):
    return next(step for step in job["steps"] if step.get("name") == name)


def source(name):
    text = (ROOT / f".github/workflows/{name}.md").read_text(encoding="utf-8")
    return yaml.safe_load(text.split("---", 2)[1])


class ActualOutputSourceTests(unittest.TestCase):
    def test_summary_receives_job_outcomes_without_synthetic_producer_receipts(self):
        summary = workflow("ghaw-pr-accessibility")["jobs"]["final-summary"]
        self.assertEqual("${{ needs.agent.result }}", summary["with"]["deep-result"])
        steps = workflow("ghaw-accessibility-summary")["jobs"]["agent"]["steps"]
        native = step_named({"steps": steps}, "Read native Actions producer outcomes")
        self.assertIn("run_id: context.runId", native["with"]["script"])
        self.assertIn("filter: 'all'", native["with"]["script"])
        self.assertIn("JSON.stringify(jobs)", native["with"]["script"])
        self.assertEqual("${{ needs.triage-route.result }}", summary["with"]["triage-result"])
        self.assertEqual("${{ needs.safe_outputs.result }}", summary["with"]["publication-result"])
        instructions = (ROOT / ".github/workflows/ghaw-accessibility-summary.md").read_text(encoding="utf-8")
        self.assertIn("No result", instructions)
        self.assertIn("publication", instructions)
        self.assertNotIn("-producer.txt", instructions)

    def test_quick_stages_disable_existing_native_harness_retry(self):
        for name in ("ghaw-accessibility-triage", "ghaw-accessibility-summary"):
            self.assertEqual("0", source(name)["env"]["GH_AW_HARNESS_MAX_RETRIES"])

    def test_all_stages_validate_actual_files_even_after_crash_before_upload(self):
        for name, stage in (("ghaw-accessibility-triage", "triage"),
                            ("ghaw-pr-accessibility", "deep"),
                            ("ghaw-accessibility-summary", "summary")):
            with self.subTest(stage=stage):
                steps = source(name)["post-steps"]
                check = next(step for step in steps if step.get("id") == "actual-output")
                upload = next(step for step in steps if "upload-artifact@" in step.get("uses", ""))
                self.assertEqual("always()", check["if"])
                self.assertIn(f'accessibility_handoff.py" {stage}', check["run"])
                self.assertIn('--producer-outcome "$PRODUCER_OUTCOME"', check["run"])
                self.assertIn('--safe-outputs "$GH_AW_SAFE_OUTPUTS"', check["run"])
                self.assertEqual("${{ steps.agentic_execution.outcome }}", check["env"]["PRODUCER_OUTCOME"])
                self.assertLess(steps.index(check), steps.index(upload))
                self.assertEqual("always()", upload["if"])
                self.assertNotIn("-producer.txt", upload["with"]["path"])
        final = source("ghaw-accessibility-summary")["post-steps"]
        publish = step_named({"steps": final}, "Publish agent-written final summary")
        self.assertIn("steps.actual-output.outcome == 'success'", publish["if"])
        self.assertNotIn("fallback", publish["run"])

    def test_reuses_native_zip_and_incomplete_machinery_without_custom_archiving(self):
        for name in ("ghaw-accessibility-triage", "ghaw-pr-accessibility", "ghaw-accessibility-summary"):
            job = workflow(name)["jobs"]["agent"]
            steps = job["steps"]
            native = step_named(job, "Upload agent artifacts")
            fallback = step_named(job, "Upload agent output fallback artifact")
            self.assertEqual("always()", native["if"])
            self.assertTrue(native["continue-on-error"])
            self.assertIn("sandbox/agent/logs/", native["with"]["path"])
            self.assertIn("agent-stdio.log", native["with"]["path"])
            self.assertIn("agent_output.json", fallback["with"]["path"])
            self.assertNotIn("final.json", fallback["with"]["path"])
            copier = step_named(job, "Copy Copilot session state files to logs")
            redactor = step_named(job, "Redact secrets in logs")
            self.assertLess(steps.index(copier), steps.index(redactor))
            self.assertLess(steps.index(redactor), steps.index(native))
            for step in source(name)["post-steps"]:
                self.assertNotIn("zipfile", step.get("run", ""))
                self.assertNotIn("Compress-Archive", step.get("run", ""))
                if "upload-artifact@" in step.get("uses", ""):
                    self.assertNotIn("session-state", step["with"]["path"])
                    self.assertNotIn("agent-stdio", step["with"]["path"])


class PartialRerunTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.main = workflow("ghaw-pr-accessibility")["jobs"]
        cls.triage = workflow("ghaw-accessibility-triage")["jobs"]["agent"]
        cls.summary = workflow("ghaw-accessibility-summary")["jobs"]["agent"]

    def test_retained_attempt_one_triage_is_downloaded_by_attempt_two_consumers(self):
        producer = step_named(self.triage, "Preserve original triage even when invalid")
        stored_name = expand(producer["with"]["name"], 1)
        consumers = (
            next(step for step in self.main["triage-route"]["steps"]
                 if "download-artifact@" in step.get("uses", "")),
            step_named(self.main["agent"], "Download original triage"),
        )
        for consumer in consumers:
            with self.subTest(consumer=consumer):
                self.assertEqual(stored_name, expand(consumer["with"]["name"], 2))
                # Domain working directories remain attempt-specific.
                self.assertIn("${{ github.run_attempt }}", consumer["with"]["path"])

    def test_retained_deep_is_available_to_retried_summary(self):
        producer = step_named(self.main["agent"], "Upload deep review evidence even when incomplete")
        deep_name = expand(producer["with"]["name"], 1)
        self.assertIn("/native-checks.md", producer["with"]["path"])
        summary = step_named(self.summary, "Download review handoffs")
        triage = step_named(self.triage, "Preserve original triage even when invalid")
        for name in (deep_name, expand(triage["with"]["name"], 1)):
            self.assertTrue(fnmatchcase(name, expand(summary["with"]["pattern"], 2)))
        self.assertFalse(fnmatchcase("accessibility-deep-54321",
                                    expand(summary["with"]["pattern"], 2)))

    def test_partial_ai_rerun_keeps_the_exact_admitted_artifact_for_both_native_consumers(self):
        self.assertEqual("${{ needs.admission.outputs.native_artifact_id }}",
                         self.main["final-summary"]["with"]["native-artifact-id"])
        for job, expression in (
            (self.main["agent"], "${{ needs.admission.outputs.native_artifact_id }}"),
            (self.summary, "${{ inputs.native-artifact-id }}"),
        ):
            native = step_named(job, "Download original-head CI native evidence")
            self.assertRegex(native["uses"], r"^actions/download-artifact@[0-9a-f]{40}$")
            self.assertEqual(expression, native["with"]["artifact-ids"])
            self.assertNotIn("name", native["with"])
            for attempt in (1, 2):
                self.assertEqual("105", expand(native["with"]["artifact-ids"], attempt).replace(expression, "105"))
                self.assertEqual("${{ github.event.inputs.ci_run_id }}", native["with"]["run-id"])
                self.assertIn("${{ github.run_attempt }}", native["with"]["path"])

    def test_rerun_replaces_stage_artifact_even_after_incomplete_production(self):
        for job, upload_name in (
            (self.triage, "Preserve original triage even when invalid"),
            (self.main["agent"], "Upload deep review evidence even when incomplete"),
        ):
            upload = step_named(job, upload_name)
            check = next(step for step in job["steps"] if step.get("id") == "actual-output")
            with self.subTest(upload=upload_name):
                self.assertIn("always()", upload["if"])
                self.assertTrue(upload["with"]["overwrite"])
                self.assertLess(job["steps"].index(check), job["steps"].index(upload))
                # Same run/stage key is replaced, not a guessed attempt-2 key.
                artifacts = {expand(upload["with"]["name"], 1): {"attempt": 1, "report": "old"}}
                artifacts[expand(upload["with"]["name"], 2)] = {"attempt": 2}
                self.assertEqual([{"attempt": 2}], list(artifacts.values()))
                # The replacement has no stale completed report from attempt one.
                self.assertNotIn("report", next(iter(artifacts.values())))

    def test_summary_validator_is_staged_on_its_own_agent_runner(self):
        steps = self.summary["steps"]
        stage = step_named(self.summary, "Stage trusted handoff validator")
        self.assertIn("cp .github/skills/pr-accessibility/scripts/accessibility_handoff.py", stage["run"])
        self.assertLess(steps.index(stage), next(i for i, step in enumerate(steps)
                                               if step.get("id") == "agentic_execution"))


if __name__ == "__main__":
    unittest.main()
