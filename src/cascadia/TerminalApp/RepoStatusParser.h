// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace Microsoft::Terminal::RepoAwareness
{
    struct RepoFileChange
    {
        std::string path;
        std::optional<std::string> originalPath;
        char indexStatus = '.';
        char worktreeStatus = '.';
        bool untracked = false;
        bool conflicted = false;
        bool submodule = false;
    };

    struct RepoStatusSnapshot
    {
        std::optional<std::string> branch;
        std::string headOid;
        std::optional<std::string> upstream;
        uint64_t ahead = 0;
        uint64_t behind = 0;
        uint64_t modifiedCount = 0;
        uint64_t stagedCount = 0;
        uint64_t untrackedCount = 0;
        uint64_t conflictedCount = 0;
        bool detached = false;
        bool unborn = false;
        bool filesTruncated = false;
        std::vector<RepoFileChange> files;
    };

    enum class RepoStatusParseError
    {
        None,
        MissingRecordTerminator,
        MalformedHeader,
        MalformedRecord,
        MissingRenameSource,
    };

    struct RepoStatusParseResult
    {
        RepoStatusSnapshot snapshot;
        RepoStatusParseError error = RepoStatusParseError::None;

        explicit operator bool() const noexcept
        {
            return error == RepoStatusParseError::None;
        }
    };

    [[nodiscard]] RepoStatusParseResult ParseRepoStatus(std::string_view output, size_t maxRetainedFiles);
}
