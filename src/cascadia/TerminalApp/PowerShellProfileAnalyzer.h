// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PowerShellProfileAnalyzer.h
//
// Read-only, out-of-process analysis of the placement of the managed
// PowerShell shell-integration block.

#pragma once

#include <string_view>

#include "../inc/ShellIntegrationProfileHealth.h"

namespace Microsoft::Terminal::ShellIntegration::Powershell::ProfileAnalyzer
{
    // `hostPath` must name the direct root PowerShell host which owns the
    // profile (powershell.exe or pwsh.exe), as an absolute path. This API
    // never resolves a host through PATH and never executes `profileBytes`.
    [[nodiscard]] Health::AnalysisResult Analyze(
        std::wstring_view hostPath,
        std::string_view profileBytes) noexcept;
}
