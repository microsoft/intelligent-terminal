// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// FreProblemKind.h
//
// Categorization of FRE Save-path failures that need a single localized
// user-facing message. Extracted from `FreOverlay` so the soft-failure
// priority selector at `inc/FreSoftFailurePriority.h` and its unit
// tests can take this enum as an ordinary value type without dragging
// in `TerminalApp` / WinRT / XAML.
//
// `FreOverlay` re-exports this as `FreOverlay::FreProblemKind` via a
// `using` alias so the consumer code in `FreOverlay.cpp` (the
// `_ShowProblem` switch + each call site that names a specific kind)
// stays binary-compatible.
//
// WinGet install failures are NOT in this enum because they carry
// richer structured state (package + failure kind + HRESULT + installer
// exit code); those go through `_ShowWingetProblem` instead, which
// uses `FreWingetFailureKind` from `inc/FreWingetClassifier.h`.

#pragma once

#include <cstdint>

namespace Microsoft::Terminal::FreProblem
{
    // Numeric order is meaningful: it is the soft-failure priority used
    // by `inc/FreSoftFailurePriority.h` to decide which problem to
    // surface when more than one fires on the same Save attempt. Lower
    // numeric values are higher priority. Reordering is a contract
    // change — `static_assert`s in the priority header pin the order
    // so a reorder breaks compilation, not silently swaps user-visible
    // behavior.
    //
    // The kinds split into two groups:
    //   * WingetMissing — an EARLY hard gate handled directly in
    //     `_SaveAndInstallAsync` before any winget call. Aborts the
    //     Save flow on hit; not part of the soft-failure ladder.
    //   * The remaining three — soft failures fired after install /
    //     hook attempts. The Save flow stops on hit and the affected
    //     feature is toggled off so the next Save click can complete.
    enum class Kind : int32_t
    {
        WingetMissing = 0, // hard prerequisite — winget itself unavailable
        ShellIntegrationExecutionPolicy = 1, // optional feature — error detection blocked by PowerShell execution policy
        ShellIntegration = 2, // optional feature — error detection (generic install failure)
        Hooks = 3, // optional feature — session management
    };
}
