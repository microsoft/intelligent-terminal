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

    def test_retained_deep_is_available_to_retried_selector_and_summary(self):
        producer = step_named(self.main["agent"], "Upload deep review evidence even when incomplete")
        deep_name = expand(producer["with"]["name"], 1)
        selector = next(step for step in self.main["native-selection"]["steps"]
                        if "download-artifact@" in step.get("uses", ""))
        self.assertEqual(deep_name, expand(selector["with"]["name"], 2))
        summary = step_named(self.summary, "Download review handoffs")
        triage = step_named(self.triage, "Preserve original triage even when invalid")
        for name in (deep_name, expand(triage["with"]["name"], 1)):
            self.assertTrue(fnmatchcase(name, expand(summary["with"]["pattern"], 2)))
        self.assertFalse(fnmatchcase("accessibility-deep-54321",
                                    expand(summary["with"]["pattern"], 2)))

    def test_rerun_replaces_stage_artifact_and_keeps_actual_producer_provenance(self):
        for job, upload_name, stage in (
            (self.triage, "Preserve original triage even when invalid", "triage"),
            (self.main["agent"], "Upload deep review evidence even when incomplete", "deep"),
        ):
            upload = step_named(job, upload_name)
            provenance = step_named(job, f"Record {stage} producer identity even when incomplete")
            with self.subTest(stage=stage):
                self.assertIn("always()", upload["if"])
                self.assertTrue(upload["with"]["overwrite"])
                self.assertIn("always()", provenance["if"])
                self.assertLess(job["steps"].index(provenance), job["steps"].index(upload))
                self.assertIn("mkdir -p", provenance["run"])
                for field in ("run=$GITHUB_RUN_ID", "attempt=$GITHUB_RUN_ATTEMPT",
                              "head=$PRODUCER_HEAD", "workflow=$PRODUCER_WORKFLOW"):
                    self.assertIn(field, provenance["run"])
                self.assertEqual("${{ github.event.pull_request.head.sha }}",
                                 provenance["env"]["PRODUCER_HEAD"])
                self.assertIn(f"/{stage}-producer.txt", provenance["run"])
                # Same run/stage key is replaced, not a guessed attempt-2 key.
                artifacts = {expand(upload["with"]["name"], 1): {"attempt": 1, "report": "old"}}
                artifacts[expand(upload["with"]["name"], 2)] = {"attempt": 2}
                self.assertEqual([{"attempt": 2}], list(artifacts.values()))
                # A failed producer still uploads provenance, not old success evidence.
                self.assertNotIn("report", next(iter(artifacts.values())))


if __name__ == "__main__":
    unittest.main()
