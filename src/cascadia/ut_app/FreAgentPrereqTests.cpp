// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// FreAgentPrereqTests.cpp
//
// Pure-function tests for the FRE agent-prerequisite predicates at
// `src/cascadia/inc/FreAgentPrereq.h`. The FRE Save path uses these
// gates to decide whether to invoke `_WingetInstallAsync` for the
// Copilot CLI or Node.js LTS based on the selected agent ID. Pinning
// these decisions here catches the class of bug where someone adds a
// new ACP agent ID (or refactors the comparison) and accidentally
// triggers a winget install that the agent does not need — wasting
// 3-20s on the user's first-run and (worse) potentially landing them
// in a winget-failure state for an install that should never have
// started.
//
// No XAML, no winrt, no filesystem — `std::wstring_view` in, `bool`
// out.

#include "precomp.h"

#include "../inc/FreAgentPrereq.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace Microsoft::Terminal::FreAgentPrereq;

namespace TerminalAppUnitTests
{
    class FreAgentPrereqTests
    {
        TEST_CLASS(FreAgentPrereqTests);

        // AgentNeedsNodeRuntime — pure agent-ID predicate, no install state.
        TEST_METHOD(NodeRuntimeRequiredByClaude);
        TEST_METHOD(NodeRuntimeRequiredByCodex);
        TEST_METHOD(NodeRuntimeNotRequiredByCopilot);
        TEST_METHOD(NodeRuntimeNotRequiredByGemini);
        TEST_METHOD(NodeRuntimeNotRequiredByEmptyOrUnknownAgent);

        // ShouldInstallNodeFor — combined gate (agent ID × install state).
        TEST_METHOD(InstallNodeForClaudeOnlyWhenMissing);
        TEST_METHOD(InstallNodeForCodexOnlyWhenMissing);
        TEST_METHOD(NeverInstallNodeForCopilotEvenWhenMissing);
        TEST_METHOD(NeverInstallNodeForGeminiEvenWhenMissing);
        TEST_METHOD(NeverInstallNodeForUnknownAgent);

        // ShouldInstallCopilotFor — combined gate (agent ID × install state).
        TEST_METHOD(InstallCopilotForCopilotOnlyWhenMissing);
        TEST_METHOD(NeverInstallCopilotForClaude);
        TEST_METHOD(NeverInstallCopilotForCodex);
        TEST_METHOD(NeverInstallCopilotForGemini);
        TEST_METHOD(NeverInstallCopilotForUnknownAgent);
    };

    // ── AgentNeedsNodeRuntime ───────────────────────────────────────────

    void FreAgentPrereqTests::NodeRuntimeRequiredByClaude()
    {
        // Claude is wired through the @zed-industries/claude-code-acp
        // npm adapter — without a Node runtime there is no CLI to launch.
        VERIFY_IS_TRUE(AgentNeedsNodeRuntime(L"claude"));
    }

    void FreAgentPrereqTests::NodeRuntimeRequiredByCodex()
    {
        // Codex is also wired through an npm acp adapter; same reasoning.
        VERIFY_IS_TRUE(AgentNeedsNodeRuntime(L"codex"));
    }

    void FreAgentPrereqTests::NodeRuntimeNotRequiredByCopilot()
    {
        // Copilot CLI ships as a native binary and speaks ACP directly.
        // If this assertion ever flips to true, the FRE Save path would
        // start installing Node.js LTS for Copilot users — a measurable
        // first-run regression (one extra ~80MB download + 20s install).
        VERIFY_IS_FALSE(AgentNeedsNodeRuntime(L"copilot"));
    }

    void FreAgentPrereqTests::NodeRuntimeNotRequiredByGemini()
    {
        // Gemini speaks ACP natively per `AgentRegistry.h`. The doc
        // explicitly calls this out as a "no Node" agent; pin it here
        // so the comment doesn't drift out of sync with code.
        VERIFY_IS_FALSE(AgentNeedsNodeRuntime(L"gemini"));
    }

    void FreAgentPrereqTests::NodeRuntimeNotRequiredByEmptyOrUnknownAgent()
    {
        // Defensive default: any agent ID we don't recognize must NOT
        // trigger a Node install. Custom agents and any future
        // misspelling / corruption in the saved settings must fall
        // through to "no install".
        VERIFY_IS_FALSE(AgentNeedsNodeRuntime(L""));
        VERIFY_IS_FALSE(AgentNeedsNodeRuntime(L"unknown-agent"));
        VERIFY_IS_FALSE(AgentNeedsNodeRuntime(L"Claude")); // case-sensitive — IDs are lowercase
        VERIFY_IS_FALSE(AgentNeedsNodeRuntime(L"claude-code")); // not a registered ID
    }

    // ── ShouldInstallNodeFor ────────────────────────────────────────────

    void FreAgentPrereqTests::InstallNodeForClaudeOnlyWhenMissing()
    {
        // Saved Claude + Node not on PATH → install. Already installed → no-op.
        VERIFY_IS_TRUE(ShouldInstallNodeFor(L"claude", /*nodeAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallNodeFor(L"claude", /*nodeAlreadyInstalled*/ true));
    }

    void FreAgentPrereqTests::InstallNodeForCodexOnlyWhenMissing()
    {
        VERIFY_IS_TRUE(ShouldInstallNodeFor(L"codex", /*nodeAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallNodeFor(L"codex", /*nodeAlreadyInstalled*/ true));
    }

    void FreAgentPrereqTests::NeverInstallNodeForCopilotEvenWhenMissing()
    {
        // Hard contract pinned by the release checklist (item: "NodeJS
        // install only triggers for Claude/Codex"). Even when Node is
        // missing — which it usually IS on a fresh box — selecting
        // Copilot must never trigger a Node install. Both arms of the
        // install-state input proven here.
        VERIFY_IS_FALSE(ShouldInstallNodeFor(L"copilot", /*nodeAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallNodeFor(L"copilot", /*nodeAlreadyInstalled*/ true));
    }

    void FreAgentPrereqTests::NeverInstallNodeForGeminiEvenWhenMissing()
    {
        // Same contract for Gemini — speaks ACP natively, no Node need.
        VERIFY_IS_FALSE(ShouldInstallNodeFor(L"gemini", /*nodeAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallNodeFor(L"gemini", /*nodeAlreadyInstalled*/ true));
    }

    void FreAgentPrereqTests::NeverInstallNodeForUnknownAgent()
    {
        // Defensive default for empty / corrupt / future-unrecognized
        // agent IDs — same reasoning as NodeRuntimeNotRequiredByEmpty…
        // but pinned again on the combined gate so a regression that
        // changes ONLY the combined gate (and not the simple predicate)
        // still surfaces.
        VERIFY_IS_FALSE(ShouldInstallNodeFor(L"", /*nodeAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallNodeFor(L"unknown-agent", /*nodeAlreadyInstalled*/ false));
    }

    // ── ShouldInstallCopilotFor ─────────────────────────────────────────

    void FreAgentPrereqTests::InstallCopilotForCopilotOnlyWhenMissing()
    {
        // Saved Copilot + Copilot CLI not on PATH → install. Already
        // installed → no-op. This is the only path that triggers a
        // GitHub.Copilot winget install.
        VERIFY_IS_TRUE(ShouldInstallCopilotFor(L"copilot", /*copilotAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"copilot", /*copilotAlreadyInstalled*/ true));
    }

    void FreAgentPrereqTests::NeverInstallCopilotForClaude()
    {
        // Selecting Claude must never trigger a Copilot CLI install,
        // regardless of whether Copilot is already on the box (the
        // selected agent doesn't use Copilot at all).
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"claude", /*copilotAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"claude", /*copilotAlreadyInstalled*/ true));
    }

    void FreAgentPrereqTests::NeverInstallCopilotForCodex()
    {
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"codex", /*copilotAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"codex", /*copilotAlreadyInstalled*/ true));
    }

    void FreAgentPrereqTests::NeverInstallCopilotForGemini()
    {
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"gemini", /*copilotAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"gemini", /*copilotAlreadyInstalled*/ true));
    }

    void FreAgentPrereqTests::NeverInstallCopilotForUnknownAgent()
    {
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"", /*copilotAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"unknown-agent", /*copilotAlreadyInstalled*/ false));
        VERIFY_IS_FALSE(ShouldInstallCopilotFor(L"Copilot", /*copilotAlreadyInstalled*/ false)); // case-sensitive
    }
}
