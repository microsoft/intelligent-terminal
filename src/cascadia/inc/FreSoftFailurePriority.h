// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// FreSoftFailurePriority.h
//
// Pure-function helper used by `FreOverlay::_SaveAndInstallAsync` to
// pick which soft-failure problem to surface when more than one fires
// on the same Save attempt. Extracted from the inline ternary at the
// failure-surfacing site so the precedence contract can be unit-tested
// without a TerminalApp / WinRT / XAML dependency.
//
// Soft-failure precedence (highest first):
//   1. ShellIntegrationExecutionPolicy — when the user's PowerShell
//      execution policy actively blocks the shell integration install,
//      we surface this specifically because the actionable message
//      mentions execution policy and the fix command. It supersedes
//      the generic ShellIntegration message even when the install
//      also failed for the "generic" reason.
//   2. ShellIntegration — generic shell-integration install failure
//      (pwsh7 or Windows PowerShell), without an execution-policy
//      cause being detected.
//   3. Hooks — agent-hook install failure (Session Management toggle).
//
// WingetMissing is NOT in this set — it is an EARLY hard gate checked
// before any winget call in `_SaveAndInstallAsync` and aborts the
// Save flow on hit. The soft-failure ladder fires only after the
// winget install path has run.
//
// The caller's responsibility: establish that at least one of the
// three input flags is true before calling this helper (i.e. there
// IS a soft failure to surface). The helper does not enforce this
// invariant; calling it with all-false inputs returns the lowest-
// priority kind (Hooks), which is meaningless without a hook failure
// having actually occurred.

#pragma once

#include "FreProblemKind.h"

namespace Microsoft::Terminal::FreSoftFailure
{
    // Static-assert the precedence is encoded in the enum's numeric
    // order. A future contributor reordering `FreProblem::Kind`
    // (e.g. moving Hooks above ShellIntegration) would silently
    // change which message users see for an unchanged failure;
    // pinning the order at compile time makes such a reorder a
    // build break instead.
    static_assert(static_cast<int>(FreProblem::Kind::WingetMissing) <
                      static_cast<int>(FreProblem::Kind::ShellIntegrationExecutionPolicy),
                  "WingetMissing must precede ShellIntegrationExecutionPolicy");
    static_assert(static_cast<int>(FreProblem::Kind::ShellIntegrationExecutionPolicy) <
                      static_cast<int>(FreProblem::Kind::ShellIntegration),
                  "ShellIntegrationExecutionPolicy must precede ShellIntegration");
    static_assert(static_cast<int>(FreProblem::Kind::ShellIntegration) <
                      static_cast<int>(FreProblem::Kind::Hooks),
                  "ShellIntegration must precede Hooks");

    // Select the highest-priority soft-failure problem to surface.
    // Caller is expected to have established `shellIntegExecutionPolicyBlocked ||
    // shellIntegrationFailed || hooksFailed` before calling.
    inline FreProblem::Kind SelectHighestPriority(
        bool shellIntegExecutionPolicyBlocked,
        bool shellIntegrationFailed,
        bool hooksFailed) noexcept
    {
        // ExecutionPolicy supersedes generic ShellIntegration even
        // when the install also reports a generic failure, because
        // the EP message carries a more actionable hint (the
        // `Set-ExecutionPolicy` fix command).
        if (shellIntegExecutionPolicyBlocked)
        {
            return FreProblem::Kind::ShellIntegrationExecutionPolicy;
        }
        if (shellIntegrationFailed)
        {
            return FreProblem::Kind::ShellIntegration;
        }
        // Caller ensures `hooksFailed` is true on this branch.
        (void)hooksFailed;
        return FreProblem::Kind::Hooks;
    }
}
