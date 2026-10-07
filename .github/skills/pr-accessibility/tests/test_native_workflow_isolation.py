"""Guard the reusable native workflow's credential and identity boundary."""
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).parents[4]


class NativeWorkflowIsolationTests(unittest.TestCase):
    def test_compiled_candidate_execution_is_a_secret_free_reusable_call(self):
        compiled = (ROOT / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        native = re.split(r"\n  [a-zA-Z][\w-]*:\n",
                          compiled.split("\n  native-runtime:\n", 1)[1], maxsplit=1)[0]
        self.assertIn("uses: ./.github/workflows/native-accessibility.yml", native)
        self.assertNotIn("secrets:", native)
        self.assertNotIn("env:", native)
        self.assertNotIn("steps:", native)
        self.assertNotIn("runs-on:", native)
        self.assertIn("contents: read", native)
        self.assertNotIn(": write", native)

    def test_reusable_native_definition_has_no_telemetry_or_secret_references(self):
        native = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_call:", native)
        self.assertNotIn("secrets:", native)
        self.assertNotIn("secrets.", native)
        self.assertNotIn("OTEL_EXPORTER_OTLP_HEADERS", native)
        self.assertNotIn("GH_AW_OTLP_ENDPOINTS", native)
        self.assertNotIn("environment:", native)
        self.assertNotIn(": write", native)

    def test_all_four_immutable_identities_are_forwarded(self):
        caller = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        native = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        for key in ("head-repository", "head-sha", "trusted-repository", "trusted-sha"):
            self.assertIn(f"      {key}:", caller)
            self.assertRegex(native, rf"{key}:\n\s+required: true\n\s+type: string")
            self.assertIn(f"${{{{ inputs.{key} }}}}", native)
        self.assertNotIn("github.event.pull_request", native)

    def test_native_step_execution_is_not_duplicated_in_the_privileged_caller(self):
        caller = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertNotIn("call tools\\razzle.cmd", caller)
        self.assertNotIn("-Surface fre-settings", caller)
        native = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        self.assertIn("call tools\\razzle.cmd", native)
        for surface in ("fre", "fre-settings", "agents"):
            self.assertIn(f"-Surface {surface}", native)
        self.assertIn("timeout-minutes: 45", native)

    def test_instruction_and_configuration_trigger_scope_is_complete(self):
        caller = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        trigger = caller.split("\npermissions:", 1)[0]
        for pattern in (".github/**", "**/.github/**", "**/AGENTS.md", "**/CLAUDE.md",
                        "**/GEMINI.md", "**/copilot-instructions.md", "**/SKILL.md",
                        "**/*.instructions.md", "**/*.agent.md", "**/.mcp.json",
                        "**/.claude/**", "**/.copilot/**", "**/.agents/**", "**/.gemini/**"):
            self.assertIn(f"'{pattern}'", trigger)
        self.assertIn("'.github/workflows/native-accessibility.yml'", trigger)


if __name__ == "__main__":
    unittest.main()
