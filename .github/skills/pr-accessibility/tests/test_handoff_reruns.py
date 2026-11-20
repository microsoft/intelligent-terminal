"""Compiled producer/consumer contracts for retained jobs on partial reruns.

These simulate artifact naming/lifecycle, not GitHub-hosted rerun execution.
"""
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
    def test_only_successfully_delivered_markdown_is_displayed_verbatim(self):
        jobs = workflow("ghaw-pr-accessibility")["jobs"]
        self.assertEqual(["triage"], [key for key, job in jobs.items() if "uses" in job])
        route = next(step for step in jobs["triage-route"]["steps"] if step.get("id") == "route")
        self.assertIn("set -euo pipefail", route["run"])
        self.assertLess(route["run"].index("accessibility_handoff.py triage"),
                        route["run"].index('cat "$handoff" >> "$GITHUB_STEP_SUMMARY"'))
        deep = jobs["agent"]
        display = step_named(deep, "Display delivered deep review")
        self.assertIn("steps.actual-output.outcome == 'success'", display["if"])
        self.assertEqual('cat "Generated Files/$ACCESSIBILITY_RUN/deep-summary.md" >> "$GITHUB_STEP_SUMMARY"',
                         display["run"])
        gate = step_named(deep, "Check findings and repair publication policy")
        self.assertLess(deep["steps"].index(display), deep["steps"].index(gate))

    def test_stages_disable_existing_native_harness_retry(self):
        for name in ("ghaw-accessibility-triage", "ghaw-pr-accessibility"):
            self.assertEqual("0", source(name)["env"]["GH_AW_HARNESS_MAX_RETRIES"])

    def test_all_stages_validate_actual_files_even_after_crash_before_upload(self):
        for name, stage in (("ghaw-accessibility-triage", "triage"),
                            ("ghaw-pr-accessibility", "deep")):
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

    def test_reuses_native_zip_and_incomplete_machinery_without_custom_archiving(self):
        for name in ("ghaw-accessibility-triage", "ghaw-pr-accessibility"):
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

if __name__ == "__main__":
    unittest.main()
