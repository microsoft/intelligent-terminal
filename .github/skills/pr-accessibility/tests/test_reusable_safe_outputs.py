"""Keep Markdown-only reusable reviewers from publishing repository issues."""
from pathlib import Path
import json
import unittest

import yaml


ROOT = Path(__file__).parents[4]
WORKFLOWS = ("ghaw-accessibility-triage", "ghaw-accessibility-summary")


class ReusableSafeOutputTests(unittest.TestCase):
    def workflows(self):
        for name in WORKFLOWS:
            path = ROOT / ".github" / "workflows" / f"{name}.lock.yml"
            yield name, path.read_text(encoding="utf-8")

    def test_issue_tool_is_not_exposed(self):
        for name, text in self.workflows():
            with self.subTest(workflow=name):
                manifest_line = next(line for line in text.splitlines()
                                     if line.startswith("# gh-aw-manifest: "))
                manifest = json.loads(manifest_line.removeprefix("# gh-aw-manifest: "))
                tools = next(server["tools"] for server in manifest["mcp_servers"]
                             if server["name"] == "safeoutputs")
                self.assertNotIn("create_issue", tools)
                self.assertIn("noop", tools)
                self.assertNotIn("safe_outputs_auto_create_issue.md", text)

    def test_agent_and_publisher_configs_have_no_issue_handlers(self):
        for name, text in self.workflows():
            workflow = yaml.safe_load(text)
            configs = {}
            for job in workflow["jobs"].values():
                for step in job.get("steps", []):
                    for key, value in step.get("env", {}).items():
                        if key in ("GH_AW_SAFE_OUTPUTS_CONFIG",
                                   "GH_AW_SAFE_OUTPUTS_HANDLER_CONFIG"):
                            configs[key] = json.loads(value)
            with self.subTest(workflow=name):
                self.assertEqual(
                    {"GH_AW_SAFE_OUTPUTS_CONFIG", "GH_AW_SAFE_OUTPUTS_HANDLER_CONFIG"},
                    configs.keys())
                for config in configs.values():
                    self.assertNotIn("create_issue", config)
                    self.assertNotIn("create_report_incomplete_issue", config)
                    self.assertIn("report_incomplete", config)
                for job in workflow["jobs"].values():
                    self.assertNotEqual("write", job.get("permissions", {}).get("issues"))

    def test_conclusion_handlers_cannot_create_issues(self):
        disabled = {
            "GH_AW_NOOP_REPORT_AS_ISSUE",
            "GH_AW_MISSING_TOOL_CREATE_ISSUE",
            "GH_AW_REPORT_INCOMPLETE_CREATE_ISSUE",
            "GH_AW_FAILURE_REPORT_AS_ISSUE",
        }
        for name, text in self.workflows():
            workflow = yaml.safe_load(text)
            env = {
                key: value
                for step in workflow["jobs"]["conclusion"]["steps"]
                for key, value in step.get("env", {}).items()
            }
            with self.subTest(workflow=name):
                for key in disabled:
                    self.assertEqual("false", env[key], key)
                self.assertNotIn(
                    "report_failed_jobs",
                    {step.get("id") for step in workflow["jobs"]["conclusion"]["steps"]})


if __name__ == "__main__":
    unittest.main()
