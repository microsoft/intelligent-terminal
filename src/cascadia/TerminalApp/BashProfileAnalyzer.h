// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// BashProfileAnalyzer.h
//
// Read-only tree-sitter based validation for the managed bash profile block.

#pragma once

#include <cstddef>
#include <cstdint>
#include <string_view>

#include "../inc/ShellIntegrationProfileHealth.h"

namespace Microsoft::Terminal::ShellIntegration::Bash::ProfileAnalyzer
{
    // This synchronous API accepts at most this many bytes and limits both
    // parsing time and syntax-tree traversal. The profile-health service uses
    // the same snapshot-size limit before it invokes this analyzer.
    inline constexpr size_t MaximumProfileBytes{ 1024 * 1024 };
    inline constexpr uint64_t ParseTimeoutMicroseconds{ 1'000'000 };
    inline constexpr size_t MaximumNodeCount{ 2 * MaximumProfileBytes };

    // Analyzes only the supplied bytes. It neither opens a profile file nor
    // executes Bash or any text from the profile.
    [[nodiscard]] Health::AnalysisResult Analyze(std::string_view profileBytes) noexcept;
}
