// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// FreSoftFailurePriorityTests.cpp
//
// Pure-function tests for the FRE soft-failure priority selector at
// `src/cascadia/inc/FreSoftFailurePriority.h`. The FRE Save path
// can produce multiple soft failures on a single attempt (e.g. shell
// integration install AND hooks install both failed); the selector
// decides which one to surface in the bottom-left error area.
//
// Precedence pinned by these tests:
//   ShellIntegrationExecutionPolicy > ShellIntegration > Hooks
//
// (`WingetMissing` is an EARLY hard gate handled directly in
// `_SaveAndInstallAsync` before any winget call — it's NOT part of
// the soft-failure ladder and so is NOT exercised here.)
//
// No XAML, no winrt — three bools in, FreProblem::Kind out.

#include "precomp.h"

#include "../inc/FreSoftFailurePriority.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace Microsoft::Terminal::FreSoftFailure;
using Kind = Microsoft::Terminal::FreProblem::Kind;

namespace TerminalAppUnitTests
{
    class FreSoftFailurePriorityTests
    {
        TEST_CLASS(FreSoftFailurePriorityTests);

        // Single-failure cases — only one of the three flags fires.
        TEST_METHOD(HooksAlone);
        TEST_METHOD(ShellIntegrationAlone);
        TEST_METHOD(ShellIntegrationExecutionPolicyAlone);

        // Two-failure precedence — pin which kind wins when two fire.
        TEST_METHOD(ShellIntegrationBeatsHooks);
        TEST_METHOD(ExecutionPolicyBeatsShellIntegration);
        TEST_METHOD(ExecutionPolicyBeatsHooks);

        // All-three — pin the top of the ladder.
        TEST_METHOD(ExecutionPolicyBeatsAll);
    };

    // ── Single failures ─────────────────────────────────────────────────

    void FreSoftFailurePriorityTests::HooksAlone()
    {
        // Hooks install failed; shell integration was fine. Surface Hooks.
        VERIFY_ARE_EQUAL(Kind::Hooks,
                         SelectHighestPriority(/*epBlocked*/ false,
                                               /*sIntegFailed*/ false,
                                               /*hooksFailed*/ true));
    }

    void FreSoftFailurePriorityTests::ShellIntegrationAlone()
    {
        // Shell integration install failed for a generic reason
        // (e.g. pwsh7 not on PATH, IO error); EP not specifically
        // blocked; hooks were fine. Surface ShellIntegration.
        VERIFY_ARE_EQUAL(Kind::ShellIntegration,
                         SelectHighestPriority(/*epBlocked*/ false,
                                               /*sIntegFailed*/ true,
                                               /*hooksFailed*/ false));
    }

    void FreSoftFailurePriorityTests::ShellIntegrationExecutionPolicyAlone()
    {
        // EP-blocked variant takes priority even when no generic
        // ShellIntegration failure was also detected. This pins the
        // contract that an EP-only signal still produces the EP
        // message (not, say, fall-through to Hooks).
        VERIFY_ARE_EQUAL(Kind::ShellIntegrationExecutionPolicy,
                         SelectHighestPriority(/*epBlocked*/ true,
                                               /*sIntegFailed*/ false,
                                               /*hooksFailed*/ false));
    }

    // ── Two-failure precedence ──────────────────────────────────────────

    void FreSoftFailurePriorityTests::ShellIntegrationBeatsHooks()
    {
        // Two failures on the same Save: surface the more specific
        // (and more actionable) ShellIntegration message. The Hooks
        // failure stays on the side-state ledger to retry on the next
        // Save (FreOverlay turns off the affected toggle).
        VERIFY_ARE_EQUAL(Kind::ShellIntegration,
                         SelectHighestPriority(/*epBlocked*/ false,
                                               /*sIntegFailed*/ true,
                                               /*hooksFailed*/ true));
    }

    void FreSoftFailurePriorityTests::ExecutionPolicyBeatsShellIntegration()
    {
        // When BOTH the generic ShellIntegration failure path fires
        // AND the more-specific EP-blocked detector fires, the user
        // gets the EP-specific message (which mentions the
        // `Set-ExecutionPolicy` fix command) rather than a generic
        // "install failed" message that requires them to read logs
        // to understand the cause.
        VERIFY_ARE_EQUAL(Kind::ShellIntegrationExecutionPolicy,
                         SelectHighestPriority(/*epBlocked*/ true,
                                               /*sIntegFailed*/ true,
                                               /*hooksFailed*/ false));
    }

    void FreSoftFailurePriorityTests::ExecutionPolicyBeatsHooks()
    {
        // EP-blocked + Hooks (without generic ShellIntegration). The
        // EP variant still supersedes Hooks. Catches the bug where a
        // refactor that drops the `sIntegFailed` branch accidentally
        // changes the EP→Hooks precedence too.
        VERIFY_ARE_EQUAL(Kind::ShellIntegrationExecutionPolicy,
                         SelectHighestPriority(/*epBlocked*/ true,
                                               /*sIntegFailed*/ false,
                                               /*hooksFailed*/ true));
    }

    // ── All three ───────────────────────────────────────────────────────

    void FreSoftFailurePriorityTests::ExecutionPolicyBeatsAll()
    {
        // Top of the ladder: when every soft failure fires, the EP
        // message wins. Pins the strictly-ordered precedence — no
        // unanticipated tie-breaker logic.
        VERIFY_ARE_EQUAL(Kind::ShellIntegrationExecutionPolicy,
                         SelectHighestPriority(/*epBlocked*/ true,
                                               /*sIntegFailed*/ true,
                                               /*hooksFailed*/ true));
    }
}
