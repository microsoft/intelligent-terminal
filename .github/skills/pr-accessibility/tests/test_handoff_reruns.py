"""Compiled producer/consumer contracts for retained jobs on partial reruns.

These simulate artifact naming/lifecycle, not GitHub-hosted rerun execution.
"""
from fnmatch import fnmatchcase
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import yaml

ROOT = Path(__file__).parents[4]
SPEC = importlib.util.spec_from_file_location(
    "handoff", ROOT / ".github/skills/pr-accessibility/scripts/accessibility_handoff.py")
HANDOFF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HANDOFF)


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
    def test_delivery_status_does_not_infer_missing_analysis_from_later_job_failure(self):
        for name in ("ghaw-accessibility-triage", "ghaw-pr-accessibility", "ghaw-accessibility-summary"):
            record = next(step for step in source(name)["post-steps"]
                          if step.get("name", "").startswith("Record "))
            self.assertEqual(
                "${{ steps.actual-output.outcome == 'success' && 'Complete' || 'No result' }}",
                record["env"]["PRODUCER_RESULT"])

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
                receipt = step_named({"steps": steps}, f"Record {stage} producer identity even when incomplete")
                upload = next(step for step in steps if "upload-artifact@" in step.get("uses", ""))
                self.assertEqual("always()", check["if"])
                self.assertIn(f'accessibility_handoff.py" {stage}', check["run"])
                self.assertIn('--producer-outcome "$PRODUCER_OUTCOME"', check["run"])
                self.assertIn('--safe-outputs "$GH_AW_SAFE_OUTPUTS"', check["run"])
                self.assertEqual("${{ steps.agentic_execution.outcome }}", check["env"]["PRODUCER_OUTCOME"])
                self.assertIn("'No result'", receipt["env"]["PRODUCER_RESULT"])
                self.assertLess(steps.index(check), steps.index(receipt))
                self.assertLess(steps.index(receipt), steps.index(upload))
                self.assertIn(f"/{stage}-producer.txt", upload["with"]["path"])
        final = source("ghaw-accessibility-summary")["post-steps"]
        publish = step_named({"steps": final}, "Publish agent-written final summary")
        self.assertIn("steps.actual-output.outcome == 'success'", publish["if"])
        self.assertNotIn("fallback", publish["run"])

    def test_real_native_metadata_is_retained_and_unrelated_sessions_are_not_uploaded(self):
        known = "11111111-2222-4333-8444-555555555555"
        other = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
        for name in ("ghaw-accessibility-triage", "ghaw-pr-accessibility", "ghaw-accessibility-summary"):
            record = next(step for step in source(name)["post-steps"]
                          if step.get("name", "").startswith("Record "))
            self.assertIn('accessibility_handoff.py" receipt', record["run"])
            for has_metadata in (True, False):
                with self.subTest(name=name, has_metadata=has_metadata):
                    with tempfile.TemporaryDirectory(dir=ROOT, prefix=".handoff-diagnostics-") as directory:
                        root = Path(directory)
                        native = root / "native"
                        state = native / "sandbox/agent/logs/copilot-session-state"
                        for session in (known, other):
                            (state / session).mkdir(parents=True)
                            (state / session / "events.jsonl").write_text("private fixture events\n")
                        # Do not use a usage parser's synthetic ID or an unrelated directory.
                        (native / "agent_usage.json").write_text('{"session_id":"synthetic-model-id"}')
                        if has_metadata:
                            (native / "agent-stdio.log").write_text(
                                f"[copilot-sdk-driver] [sdk-driver] session created: sessionId={known}\n")
                        stage = record["env"]["PRODUCER_STAGE"]
                        run = root / "Generated Files/accessibility-12345-2"
                        run.mkdir(parents=True)
                        env = dict(os.environ, ACCESSIBILITY_RUN="accessibility-12345-2",
                                   PRODUCER_STAGE=stage, PRODUCER_HEAD="a" * 40,
                                   GITHUB_RUN_ID="12345", GITHUB_RUN_ATTEMPT="2",
                                   PRODUCER_SOURCE=record["env"]["PRODUCER_SOURCE"],
                                   PRODUCER_WORKFLOW="b" * 40,
                                   PRODUCER_RESULT="No result", PRODUCER_OUTCOME="failure",
                                   DIAGNOSTIC_ARTIFACT="native-agent")
                        with patch.dict(os.environ, env):
                            HANDOFF.receipt(run / f"{stage}-producer.txt", native)
                        receipt = (run / f"{stage}-producer.txt").read_text()
                        self.assertIn(f"diagnostic-id={stage}-{'a' * 40}-12345-2", receipt)
                        self.assertIn(f"source={record['env']['PRODUCER_SOURCE']}", receipt)
                        self.assertIn("result=No result", receipt)
                        self.assertIn("execution=failure", receipt)
                        self.assertIn(f"sessionID={known if has_metadata else 'unknown'}\n", receipt)
                        self.assertIn("diagnostics-artifact=native-agent", receipt)
                        self.assertNotIn("synthetic-model-id", receipt)
                        self.assertFalse((state / other).exists())
                        self.assertEqual(has_metadata, (state / known).exists())
                        self.assertFalse((run / "summary.md").exists())
                        self.assertFalse((run / "final.json").exists())

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
                self.assertIn('accessibility_handoff.py" receipt', provenance["run"])
                self.assertEqual("${{ fromJSON(github.event.inputs.pull_request).head.sha }}",
                                 provenance["env"]["PRODUCER_HEAD"])
                self.assertIn(f"/{stage}-producer.txt", provenance["run"])
                # Same run/stage key is replaced, not a guessed attempt-2 key.
                artifacts = {expand(upload["with"]["name"], 1): {"attempt": 1, "report": "old"}}
                artifacts[expand(upload["with"]["name"], 2)] = {"attempt": 2}
                self.assertEqual([{"attempt": 2}], list(artifacts.values()))
                # A failed producer still uploads provenance, not old success evidence.
                self.assertNotIn("report", next(iter(artifacts.values())))

    def test_summary_validator_is_staged_on_its_own_agent_runner(self):
        steps = self.summary["steps"]
        stage = step_named(self.summary, "Stage trusted handoff validator")
        self.assertIn("cp .github/skills/pr-accessibility/scripts/accessibility_handoff.py", stage["run"])
        self.assertLess(steps.index(stage), next(i for i, step in enumerate(steps)
                                               if step.get("id") == "agentic_execution"))


if __name__ == "__main__":
    unittest.main()
