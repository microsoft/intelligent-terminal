// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "GitProcessRunner.h"
#include "RepoStatusParser.h"

namespace Microsoft::Terminal::RepoAwareness
{
    enum class LocalGitError
    {
        None,
        GitUnavailable,
        InvalidWorkingDirectory,
        NotRepository,
        TimedOut,
        Cancelled,
        OutputLimitExceeded,
        ProcessFailed,
        InvalidOutput,
    };

    struct LocalRepoSnapshot
    {
        std::filesystem::path worktreeRoot;
        RepoStatusSnapshot status;
    };

    struct LocalGitResult
    {
        LocalRepoSnapshot snapshot;
        LocalGitError error = LocalGitError::None;
        unsigned long exitCode = 0;
        unsigned long win32Error = 0;

        explicit operator bool() const noexcept
        {
            return error == LocalGitError::None;
        }
    };

    class LocalGitProvider
    {
    public:
        explicit LocalGitProvider(GitProcessRunner runner);

        [[nodiscard]] LocalGitResult Refresh(const std::filesystem::path& workingDirectory,
                                             size_t maxRetainedFiles = 500,
                                             const std::atomic_bool* cancelled = nullptr) const;

    private:
        GitProcessRunner _runner;
    };
}
