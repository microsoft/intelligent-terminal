"""CI topology and retained execution contracts; no builds or desktop interaction."""
from pathlib import Path
import unittest
import yaml

ROOT = Path(__file__).parents[4]


class CiNativeGraphTests(unittest.TestCase):
    def setUp(self):
        self.ci = yaml.safe_load((ROOT / ".github/workflows/pr-ci.yml").read_text(encoding="utf-8"))
        self.jobs = self.ci["jobs"]
        self.native = yaml.safe_load((ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8"))

    def test_single_build_then_sibling_consumers_with_independent_rust(self):
        self.assertEqual({"cpp", "cpp-tests", "axe", "rust"}, set(self.jobs))
        for job in ("cpp", "rust"):
            self.assertNotIn("needs", self.jobs[job])
        for job in ("cpp-tests", "axe"):
            self.assertEqual(["cpp"], self.jobs[job]["needs"])
        self.assertIn("!cancelled() && needs.cpp.outputs.units == 'true'", self.jobs["cpp-tests"]["if"])
        self.assertEqual("${{ steps.units.outcome == 'success' }}", self.jobs["cpp"]["outputs"]["units"])
        self.assertEqual("needs.cpp.outputs.accessibility == 'true'", self.jobs["axe"]["if"])
        self.assertEqual("${{ steps.accessibility-scope.outputs.required }}", self.jobs["cpp"]["outputs"]["accessibility"])
        self.assertEqual("./.github/workflows/native-accessibility.yml", self.jobs["axe"]["uses"])
        self.assertNotIn("secrets", self.jobs["axe"])
        self.assertEqual({"native-smoke"}, set(self.native["jobs"]))
        self.assertNotIn("needs", self.native["jobs"]["native-smoke"])
        build = self.jobs["cpp"]["steps"]
        names = [s.get("name") for s in build]
        self.assertNotIn("Test all C++ units", names)
        self.assertLess(names.index("Build production Terminal and C++ tests"),
                        names.index("Stage ordinary C++ unit test payload"))
        self.assertLess(names.index("Stage complete original-head TestHost layout"),
                        names.index("Publish accessibility TestHost payload"))
        for job in ("cpp-tests",):
            self.assertFalse(any("MSBUILD" in s.get("run", "") or "razzle.cmd" in s.get("run", "")
                                 for s in self.jobs[job]["steps"]))

    def test_original_tests_and_security_artifacts_remain_required(self):
        tests = next(s for s in self.jobs["cpp-tests"]["steps"] if s.get("name") == "Test all C++ units")
        for name in ("Conhost.Unit.Tests.dll", "Conhost.Interactivity.Win32.Unit.Tests.dll", "til.unit.tests.dll",
                     "Types.Unit.Tests.dll", "TextBuffer.Unit.Tests.dll", "ConAdapter.Unit.Tests.dll",
                     "ConParser.Unit.Tests.dll", "Control.Unit.Tests.dll", "SettingsModel.Unit.Tests.dll",
                     "Terminal.Core.Unit.Tests.dll", "Terminal.App.Unit.Tests.dll"):
            self.assertIn(name, tests["run"])
        self.assertIn("-MatchPattern '*unit.test*.dll'", tests["run"])
        self.assertIn("'/testTimeout:0:00:30','/sessionTimeout:0:15:00'", tests["run"])
        rust = next(s for s in self.jobs["rust"]["steps"] if s.get("name") == "Test all x64 Rust units")
        self.assertIn("tools\\wta\\Cargo.toml", rust["run"])
        self.assertIn("installer\\bootstrap\\Cargo.toml", rust["run"])
        self.assertIn("test --locked --bins --target x86_64-pc-windows-msvc", rust["run"])
        for job, language in (("cpp", "cpp"), ("rust", "rust")):
            uploads = [s for s in self.jobs[job]["steps"] if s.get("name") == f"Upload {language.title() if language == 'rust' else 'C++'} security evidence"]
            self.assertEqual(1, len(uploads))
            self.assertIn(f"ci-security-{language}-", uploads[0]["with"]["name"])
        tests_names = [s.get("name") for s in self.jobs["cpp-tests"]["steps"]]
        self.assertLess(tests_names.index("Verify unit payload identity and completeness"), tests_names.index("Test all C++ units"))

    def test_failed_or_missing_native_receipts_are_uploaded_before_enforcement(self):
        steps = self.native["jobs"]["native-smoke"]["steps"]
        names = [s.get("name") for s in steps]
        for name in ("Scan FRE", "Scan Agents settings", "Scan FRE settings", "Scan Sidebar"):
            self.assertTrue(next(s for s in steps if s.get("name") == name)["continue-on-error"])
        for name in ("Record native CI results", "Upload native accessibility evidence", "Enforce native scan results"):
            self.assertEqual("always()", next(s for s in steps if s.get("name") == name)["if"])
        self.assertLess(names.index("Record native CI results"), names.index("Upload native accessibility evidence"))
        self.assertLess(names.index("Upload native accessibility evidence"), names.index("Enforce native scan results"))
        upload = next(s for s in steps if s.get("name") == "Upload native accessibility evidence")
        self.assertEqual("error", upload["with"]["if-no-files-found"])
        self.assertIn("ci-accessibility-native-", upload["with"]["name"])
        self.assertEqual("${{ github.workflow_sha }}", self.jobs["axe"]["with"]["trusted-sha"])

    def test_native_and_ordinary_ci_execution_actions_are_immutable(self):
        for job in [*self.jobs.values(), *self.native["jobs"].values()]:
            for step in job.get("steps", []):
                if "uses" in step:
                    self.assertRegex(step["uses"], r"@[0-9a-f]{40}$")

    def test_partial_consumer_reruns_use_actual_payload_producer_attempt(self):
        self.assertEqual("${{ github.run_attempt }}", self.jobs["cpp"]["outputs"]["payload-attempt"])
        steps = self.jobs["cpp-tests"]["steps"]
        download = next(s for s in steps if s.get("name") == "Download original build unit payload")
        verify = next(s for s in steps if s.get("name") == "Verify unit payload identity and completeness")
        self.assertEqual("ci-cpp-units-${{ github.run_id }}-${{ needs.cpp.outputs.payload-attempt }}",
                         download["with"]["name"])
        self.assertEqual("${{ needs.cpp.outputs.payload-attempt }}", verify["env"]["CI_RUN_ATTEMPT"])
        self.assertEqual("${{ needs.cpp.outputs.payload-attempt }}", self.jobs["axe"]["with"]["payload-attempt"])
        native = self.native["jobs"]["native-smoke"]["steps"]
        fetch = next(s for s in native if s.get("name") == "Download authenticated artifact without extracting it")
        extract = next(s for s in native if s.get("name") == "Verify archive integrity and complete portable layout before extraction")
        self.assertEqual("${{ inputs.payload-attempt }}", fetch["env"]["PAYLOAD_ATTEMPT"])
        self.assertIn("process.env.PAYLOAD_ATTEMPT", fetch["with"]["script"])
        self.assertEqual("${{ inputs.payload-attempt }}", extract["env"]["CI_RUN_ATTEMPT"])
        # Native result receipts still identify the actual native execution,
        # not the older build attempt used solely for payload verification.
        self.assertEqual("${{ github.run_attempt }}", self.native["env"]["CI_RUN_ATTEMPT"])
        current_attempt, retained_producer = 2, 1
        self.assertNotEqual(current_attempt, retained_producer)
        name = download["with"]["name"].replace("${{ github.run_id }}", "91").replace(
            "${{ needs.cpp.outputs.payload-attempt }}", str(retained_producer))
        self.assertEqual("ci-cpp-units-91-1", name)


if __name__ == "__main__":
    unittest.main()
