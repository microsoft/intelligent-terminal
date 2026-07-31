// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "LocalGitProvider.h"

namespace Microsoft::Terminal::RepoAwareness
{
    namespace
    {
        LocalGitError _mapProcessError(const GitProcessStatus status)
        {
            switch (status)
            {
            case GitProcessStatus::TimedOut:
                return LocalGitError::TimedOut;
            case GitProcessStatus::Cancelled:
                return LocalGitError::Cancelled;
            case GitProcessStatus::OutputLimitExceeded:
                return LocalGitError::OutputLimitExceeded;
            case GitProcessStatus::SpawnFailed:
                return LocalGitError::GitUnavailable;
            default:
                return LocalGitError::ProcessFailed;
            }
        }

        std::optional<std::filesystem::path> _decodeRoot(std::string_view output)
        {
            while (!output.empty() && (output.back() == '\r' || output.back() == '\n'))
            {
                output.remove_suffix(1);
            }
            if (output.empty() || output.find('\0') != std::string_view::npos)
            {
                return std::nullopt;
            }

            const auto length = MultiByteToWideChar(
                CP_UTF8,
                MB_ERR_INVALID_CHARS,
                output.data(),
                static_cast<int>(output.size()),
                nullptr,
                0);
            if (length <= 0)
            {
                return std::nullopt;
            }

            std::wstring decoded(length, L'\0');
            if (MultiByteToWideChar(
                    CP_UTF8,
                    MB_ERR_INVALID_CHARS,
                    output.data(),
                    static_cast<int>(output.size()),
                    decoded.data(),
                    length) != length)
            {
                return std::nullopt;
            }

            std::filesystem::path root{ std::move(decoded) };
            if (!root.is_absolute())
            {
                return std::nullopt;
            }

            std::error_code error;
            const auto canonical = std::filesystem::weakly_canonical(root, error);
            return error ? std::optional<std::filesystem::path>{ root.lexically_normal() } :
                           std::optional<std::filesystem::path>{ canonical };
        }
    }

    LocalGitProvider::LocalGitProvider(GitProcessRunner runner) :
        _runner{ std::move(runner) }
    {
    }

    LocalGitResult LocalGitProvider::Refresh(const std::filesystem::path& workingDirectory,
                                             const size_t maxRetainedFiles,
                                             const std::atomic_bool* cancelled) const
    {
        LocalGitResult result;
        std::error_code filesystemError;
        if (!workingDirectory.is_absolute() ||
            !std::filesystem::is_directory(workingDirectory, filesystemError) ||
            filesystemError)
        {
            result.error = LocalGitError::InvalidWorkingDirectory;
            return result;
        }
        if (!_runner.IsValid())
        {
            result.error = LocalGitError::GitUnavailable;
            return result;
        }

        GitProcessOptions rootOptions;
        rootOptions.timeout = std::chrono::seconds{ 3 };
        rootOptions.maxOutputBytes = 64 * 1024;
        rootOptions.cancelled = cancelled;
        const auto rootResult = _runner.Run(
            workingDirectory,
            {
                L"-c",
                L"core.fsmonitor=false",
                L"-c",
                L"core.quotepath=false",
                L"-C",
                workingDirectory.native(),
                L"rev-parse",
                L"--show-toplevel",
            },
            rootOptions);

        result.exitCode = rootResult.exitCode;
        result.win32Error = rootResult.win32Error;
        if (rootResult.status != GitProcessStatus::Succeeded)
        {
            result.error = rootResult.status == GitProcessStatus::Failed && rootResult.exitCode == 128 ?
                               LocalGitError::NotRepository :
                               _mapProcessError(rootResult.status);
            return result;
        }

        const auto root = _decodeRoot(rootResult.standardOutput);
        if (!root)
        {
            result.error = LocalGitError::InvalidOutput;
            return result;
        }

        GitProcessOptions statusOptions;
        statusOptions.timeout = std::chrono::seconds{ 10 };
        statusOptions.maxOutputBytes = 16 * 1024 * 1024;
        statusOptions.cancelled = cancelled;
        const auto statusResult = _runner.Run(
            *root,
            {
                L"-c",
                L"core.fsmonitor=false",
                L"-c",
                L"core.quotepath=false",
                L"-C",
                root->native(),
                L"status",
                L"--porcelain=v2",
                L"--branch",
                L"-z",
                L"--untracked-files=all",
                L"--ignore-submodules=none",
            },
            statusOptions);

        result.exitCode = statusResult.exitCode;
        result.win32Error = statusResult.win32Error;
        if (statusResult.status != GitProcessStatus::Succeeded)
        {
            result.error = _mapProcessError(statusResult.status);
            return result;
        }

        auto parsed = ParseRepoStatus(statusResult.standardOutput, maxRetainedFiles);
        if (!parsed)
        {
            result.error = LocalGitError::InvalidOutput;
            return result;
        }

        result.snapshot.worktreeRoot = *root;
        result.snapshot.status = std::move(parsed.snapshot);
        return result;
    }
}
