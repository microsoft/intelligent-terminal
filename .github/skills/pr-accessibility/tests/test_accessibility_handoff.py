"""Markdown boundary and compiled-graph regression tests, not model evaluation."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile

import yaml

ROOT = Path(__file__).parents[4]
SPEC = importlib.util.spec_from_file_location(
    "handoff", ROOT / ".github/skills/pr-accessibility/scripts/accessibility_handoff.py"
)
handoff = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(handoff)
CHANGED = {"src/cascadia/TerminalApp/Test.xaml", "src/cascadia/TerminalApp/en-GB.resw"}
SUMMARY = "## Outcome\nActual review.\n\n## Results\nNo findings.\n\n## Human action and limits\nRuntime gaps remain.\n"


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


class ProducerOutputTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.revision = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(dir=ROOT, prefix=".handoff-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.queue = self.root / "safeoutputs.jsonl"
        self.queue.write_text('{"type":"noop"}\n', encoding="utf-8")
        self.summary = self.root / "summary.md"
        self.summary.write_text(SUMMARY, encoding="utf-8")
        self.report = {
            "version": 1, "source_sha": "a" * 40, "review_checks": [],
            "runtime_checks": [], "findings": [], "dismissed_signals": [], "patch_files": [],
        }
        (self.root / "final.json").write_text(json.dumps(self.report), encoding="utf-8")
        (self.root / "deep-summary.md").write_text(SUMMARY, encoding="utf-8")
        (self.root / "native-checks.md").write_text(
            "# Native checks: None\n\nActual source-only selection.\n", encoding="utf-8")

    def run_producer(self, kind, path, outcome="success"):
        revisions = ["--base", self.revision, "--head", self.revision] if kind == "triage" else []
        return subprocess.run(
            [sys.executable, str(SPEC.origin), kind, str(path),
             "--producer-outcome", outcome, "--safe-outputs", str(self.queue), *revisions],
            capture_output=True, text=True, cwd=ROOT, check=False,
        )

    def assert_no_result(self, result):
        self.assertNotEqual(0, result.returncode)
        self.assertEqual("", result.stdout)
        self.assertIn("No result:", result.stderr)

    def test_complete_real_files_are_accepted_unchanged(self):
        for kind, path in (("summary", self.summary), ("deep", self.root)):
            with self.subTest(kind=kind):
                result = self.run_producer(kind, path)
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertEqual("Complete\n", result.stdout)
        self.assertEqual(SUMMARY, self.summary.read_text(encoding="utf-8"))
        triage = self.root / "triage.md"
        original = "# None\n\nActual source diff has no changes.\n"
        triage.write_text(original, encoding="utf-8")
        result = self.run_producer("triage", triage)
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual("None\n", result.stdout)
        self.assertEqual(original, triage.read_text(encoding="utf-8"))

    def test_crashed_cancelled_or_unknown_producer_cannot_reuse_complete_files(self):
        for kind, path in (("summary", self.summary), ("deep", self.root),
                           ("triage", self.root / "triage.md")):
            path_to_write = path if kind == "triage" else None
            if path_to_write:
                path_to_write.write_text("# None\n\nActual decision.\n", encoding="utf-8")
            for outcome in ("failure", "cancelled", "skipped", ""):
                with self.subTest(kind=kind, outcome=outcome):
                    self.assert_no_result(self.run_producer(kind, path, outcome))
        self.assertEqual(SUMMARY, self.summary.read_text(encoding="utf-8"))

    def test_complete_analysis_is_retained_when_a_later_stage_fails(self):
        triage = self.root / "triage.md"
        triage.write_text("# None\n\nActual source diff has no changes.\n", encoding="utf-8")
        for kind, path in (("triage", triage), ("deep", self.root), ("summary", self.summary)):
            with self.subTest(kind=kind):
                produced = self.run_producer(kind, path)
                self.assertEqual(0, produced.returncode, produced.stderr)
                later = subprocess.run(
                    [sys.executable, "-c", "raise RuntimeError('later validation failed')"],
                    capture_output=True, text=True, check=False)
                self.assertNotEqual(0, later.returncode)
                self.assertNotIn("No result", produced.stdout + produced.stderr)
                self.assertEqual(0, self.run_producer(kind, path).returncode)
        self.assertEqual(SUMMARY, self.summary.read_text(encoding="utf-8"))

    def test_real_producer_process_failure_leaves_diagnostics_not_a_successful_handoff(self):
        for kind, filename, content in (
            ("triage", "triage.md", "# None\n\nA file written before the crash.\n"),
            ("deep", "final.json", json.dumps(self.report)),
            ("summary", "summary.md", SUMMARY),
        ):
            with self.subTest(kind=kind):
                crashed = subprocess.run(
                    [sys.executable, "-c",
                     "from pathlib import Path; import sys; "
                     "Path(sys.argv[1]).write_text(sys.argv[2], encoding='utf-8'); "
                     "raise RuntimeError('producer crashed after writing')",
                     filename, content],
                    cwd=self.root, capture_output=True, text=True, check=False,
                )
                self.assertNotEqual(0, crashed.returncode)
                self.assertIn("producer crashed after writing", crashed.stderr)
                path = self.root if kind == "deep" else self.root / filename
                self.assert_no_result(self.run_producer(
                    kind, path, "success" if crashed.returncode == 0 else "failure"))
                self.assertEqual(content, (self.root / filename).read_text(encoding="utf-8"))

    def test_missing_empty_and_incomplete_final_outputs_never_generate_replacements(self):
        for text in (None, "", " \n", "## Outcome\nOnly a partial answer.\n",
                     "## Outcome\nActual.\n## Results\n\n## Human action and limits\nGap.\n"):
            with self.subTest(text=text):
                self.summary.unlink(missing_ok=True)
                if text is not None:
                    self.summary.write_text(text, encoding="utf-8")
                self.assert_no_result(self.run_producer("summary", self.summary))
                if text is None:
                    self.assertFalse(self.summary.exists())
                else:
                    self.assertEqual(text, self.summary.read_text(encoding="utf-8"))

    def test_deep_requires_each_actual_file_and_complete_json(self):
        for filename in ("final.json", "deep-summary.md", "native-checks.md"):
            path = self.root / filename
            original = path.read_text(encoding="utf-8")
            for text in (None, "", "{}" if filename == "final.json" else "# Incomplete\n"):
                with self.subTest(filename=filename, text=text):
                    path.unlink(missing_ok=True)
                    if text is not None:
                        path.write_text(text, encoding="utf-8")
                    self.assert_no_result(self.run_producer("deep", self.root))
            path.write_text(original, encoding="utf-8")

    def test_native_incomplete_and_missing_queue_override_plausible_files(self):
        for text in (None, "", '{"items":[]}\n', 'invalid\n',
                     '{"type":"report_incomplete","reason":"infrastructure_error"}\n',
                     '{"type":"missing_data"}\n', '{"type":"missing_tool"}\n'):
            with self.subTest(text=text):
                self.queue.unlink(missing_ok=True)
                if text is not None:
                    self.queue.write_text(text, encoding="utf-8")
                self.assert_no_result(self.run_producer("summary", self.summary))

    def test_missing_or_empty_triage_is_not_none_or_uncertain(self):
        triage = self.root / "triage.md"
        for text in (None, "", " \n"):
            with self.subTest(text=text):
                triage.unlink(missing_ok=True)
                if text is not None:
                    triage.write_text(text, encoding="utf-8")
                self.assert_no_result(self.run_producer("triage", triage))


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

    def test_native_ci_consumption_is_independent_of_deep_selection(self):
        selector = self.jobs["native-selection"]
        self.assertEqual("agent", selector["needs"])
        self.assertIn("steps.selection.outputs.native", selector["outputs"]["native"])
        native = self.jobs["native-evidence"]
        self.assertEqual("admission", native["needs"])
        self.assertIn("needs.admission.outputs.applicable == 'true'", native["if"])
        self.assertNotIn("triage", native["if"])
        self.assertIn("native-evidence", self.jobs["agent"]["needs"])
        self.assertNotIn("native-runtime", self.jobs)

    def test_native_selection_requires_complete_deep_production(self):
        selector = self.jobs["native-selection"]
        self.assertEqual(
            "needs.agent.result == 'success'",
            selector["if"],
        )
        # A file left by a crashed producer is diagnostic, not a valid decision.
        self.assertIn("needs.agent.result == 'success'", self.jobs["safe_outputs"]["if"])
        upload = next(step for step in self.jobs["agent"]["steps"]
                      if step.get("name") == "Upload deep review evidence even when incomplete")
        self.assertIn("always()", upload["if"])

    def test_final_summary_runs_after_skips_and_failures(self):
        summary = self.jobs["final-summary"]
        self.assertIn("always()", summary["if"])
        self.assertEqual(
            {"admission", "agent", "native-evidence", "native-selection", "safe_outputs", "triage", "triage-route"},
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
        for name in ("ghaw-accessibility-triage.lock.yml", "ghaw-accessibility-summary.lock.yml"):
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

    def test_native_consumer_verifies_archive_before_models_and_never_uses_repairs(self):
        consumer = self.jobs["native-evidence"]
        admission = next(step for step in consumer["steps"]
                         if step.get("uses") == "./.github/actions/accessibility-ci-admission")
        self.assertEqual("true", admission["with"]["require-native"])
        extraction = next(step for step in consumer["steps"]
                          if step.get("name") == "Verify and extract untrusted CI native evidence")
        self.assertEqual("${{ fromJSON(github.event.inputs.pull_request).head.sha }}",
                         extraction["env"]["EXPECTED_HEAD_SHA"])
        self.assertIn("ci-accessibility-payload.py extract-native", extraction["run"])
        self.assertIn('--digest "$ARTIFACT_DIGEST"', extraction["run"])
        self.assertNotIn("published-sha", str(consumer))
        self.assertEqual("ubuntu-latest", consumer["runs-on"])
        self.assertEqual("read", consumer["permissions"]["actions"])
        upload = next(step for step in consumer["steps"]
                      if step.get("name") == "Preserve verified original-head CI native evidence")
        self.assertLess(consumer["steps"].index(extraction), consumer["steps"].index(upload))
        self.assertNotIn("always()", upload.get("if", ""))
        self.assertIn("native-evidence", self.jobs["deep-admission"]["needs"])

    def test_existing_parallel_ci_build_publishes_payload_without_a_second_build(self):
        ci = yaml.safe_load((ROOT / ".github/workflows/pr-ci.yml").read_text(encoding="utf-8"))
        self.assertNotIn("needs", ci["jobs"]["cpp"])
        self.assertNotIn("needs", ci["jobs"]["rust"])
        steps = ci["jobs"]["cpp"]["steps"]
        names = [step["name"] for step in steps]
        self.assertLess(names.index("Build production Terminal and C++ tests"),
                        names.index("Stage complete original-head TestHost layout"))
        units_id, units = next((key, job) for key, job in ci["jobs"].items()
                               if job.get("name") == "C++ unit tests")
        self.assertIn("cpp", units["needs"])
        self.assertNotIn("Build production Terminal and C++ tests", str(units))
        self.assertIn("Test all C++ units", str(units))
        native = next(job for job in ci["jobs"].values() if job.get("name") == "Native accessibility")
        self.assertIn("cpp", native["needs"])
        self.assertNotIn(units_id, native["needs"])
        for name in ("Stage complete original-head TestHost layout", "Publish accessibility TestHost payload"):
            self.assertIn("steps.accessibility-scope.outputs.required == 'true'",
                          next(step for step in steps if step["name"] == name)["if"])
        self.assertNotIn("doc/**", ci[True]["pull_request"]["paths"])
        build = next(step for step in steps if step["name"] == "Build production Terminal and C++ tests")
        self.assertEqual(1, build["run"].count(r"Terminal\Tests\TestHostApp"))

    def test_controller_bot_is_authorized_but_ci_admission_still_gates_models(self):
        for name in ("ghaw-accessibility-triage.lock.yml", "ghaw-accessibility-summary.lock.yml"):
            jobs = yaml.safe_load((ROOT / ".github/workflows" / name).read_text(encoding="utf-8"))["jobs"]
            membership = next(step for step in jobs["pre_activation"]["steps"] if step.get("id") == "check_membership")
            self.assertEqual("github-actions[bot]", membership["env"]["GH_AW_ALLOWED_BOTS"])
            self.assertNotIn("read", membership["env"]["GH_AW_REQUIRED_ROLES"])
            self.assertIn("admission", jobs["agent"]["needs"])

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

    def test_model_triage_extraction_cannot_replace_verified_ci_native_evidence(self):
        steps = self.jobs["agent"]["steps"]
        native = next(s for s in steps if s.get("name") == "Download verified original-head CI native evidence")
        triage = next(s for s in steps if s.get("name") == "Download original triage")
        native_path = native["with"]["path"]
        triage_path = triage["with"]["path"]
        self.assertTrue(native_path.endswith("/native"))
        self.assertEqual(native_path.removesuffix("/native") + "/triage-input", triage_path)
        route = next(s for s in self.jobs["triage-route"]["steps"] if "download-artifact@" in s.get("uses", ""))
        self.assertEqual(triage_path, route["with"]["path"])
        producer = yaml.safe_load((ROOT / ".github/workflows/ghaw-accessibility-triage.lock.yml").read_text(
            encoding="utf-8"))["jobs"]["agent"]
        upload = next(s for s in producer["steps"] if s.get("name") == "Preserve original triage even when invalid")
        self.assertEqual(["/triage.md", "/triage-producer.txt"],
                         ["/" + line.rsplit("/", 1)[-1] for line in upload["with"]["path"].splitlines()])
        # A hostile extra native member in a retained/model-authored artifact
        # still stays in the triage namespace, even though producer scope now
        # prevents publishing it. Use the actual compiled destination suffix.
        with tempfile.TemporaryDirectory(dir=ROOT, prefix=".handoff-test-") as directory:
            root = Path(directory)
            verified = root / "native/fre/result.json"
            verified.parent.mkdir(parents=True)
            verified.write_text("verified original CI result")
            archive = root / "triage.zip"
            with zipfile.ZipFile(archive, "w") as stream:
                stream.writestr("triage.md", "# None\n\nUntrusted model handoff.\n")
                stream.writestr("native/fre/result.json", "forged PASS")
            with zipfile.ZipFile(archive) as stream:
                stream.extractall(root / triage_path.rsplit("/", 1)[-1])
            self.assertEqual("verified original CI result", verified.read_text())
            self.assertEqual("forged PASS", (root / "triage-input/native/fre/result.json").read_text())

    def test_native_selection_output_stays_at_explicit_run_root_after_triage_isolation(self):
        source = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("Write `Generated Files/${{ env.ACCESSIBILITY_RUN }}/native-checks.md` at the run root", source)
        self.assertNotIn("beside the triage", source)
        selector = next(s for s in self.jobs["native-selection"]["steps"] if s.get("id") == "selection")
        self.assertIn('"Generated Files/$ACCESSIBILITY_RUN/native-checks.md"', selector["run"])
        self.assertNotIn("/triage-input/native-checks.md", str(selector))

    def test_domain_outputs_are_generated_and_not_publication_paths(self):
        deep = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertNotIn("/tmp/gh-aw/accessibility/", deep)
        allowed = deep.split("    allowed-files:", 1)[1].split("    protected-files:", 1)[0]
        self.assertNotIn("Generated Files", allowed)
        self.assertIn("never stage or commit them", deep)
        self.assertIn("Do not build or test before committing or pushing", deep)


if __name__ == "__main__":
    unittest.main()
