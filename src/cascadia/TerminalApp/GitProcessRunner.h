// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <atomic>
#include <chrono>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace Microsoft::Terminal::RepoAwareness
{
    enum class GitProcessStatus
    {
        Succeeded,
        Failed,
        SpawnFailed,
        TimedOut,
        Cancelled,
        OutputLimitExceeded,
    };

    struct GitProcessResult
    {
        GitProcessStatus status = GitProcessStatus::SpawnFailed;
        unsigned long exitCode = 0;
        unsigned long win32Error = 0;
        std::string standardOutput;
        std::string standardError;
    };

    struct GitProcessOptions
    {
        std::chrono::milliseconds timeout{ 5000 };
        size_t maxOutputBytes = 16 * 1024 * 1024;
        const std::atomic_bool* cancelled = nullptr;
    };

    class GitProcessRunner
    {
    public:
        explicit GitProcessRunner(std::filesystem::path gitExecutable);

        [[nodiscard]] static std::optional<std::filesystem::path> FindGitExecutable();
        [[nodiscard]] bool IsValid() const noexcept;
        [[nodiscard]] GitProcessResult Run(const std::filesystem::path& workingDirectory,
                                           const std::vector<std::wstring>& arguments,
                                           const GitProcessOptions& options = {}) const;

    private:
        std::filesystem::path _gitExecutable;
    };
}
