// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "RepoStatusParser.h"

#include <charconv>

namespace Microsoft::Terminal::RepoAwareness
{
    namespace
    {
        bool _nextField(std::string_view& input, std::string_view& field, const size_t remainingFields = 1)
        {
            if (remainingFields == 1)
            {
                field = input;
                input = {};
                return !field.empty();
            }

            const auto delimiter = input.find(' ');
            if (delimiter == std::string_view::npos)
            {
                return false;
            }

            field = input.substr(0, delimiter);
            input.remove_prefix(delimiter + 1);
            return !field.empty();
        }

        bool _parseCount(std::string_view value, uint64_t& result)
        {
            if (value.empty())
            {
                return false;
            }

            const auto [end, error] = std::from_chars(value.data(), value.data() + value.size(), result);
            return error == std::errc{} && end == value.data() + value.size();
        }

        void _retainFile(RepoStatusSnapshot& snapshot, RepoFileChange&& file, const size_t maxRetainedFiles)
        {
            if (snapshot.files.size() < maxRetainedFiles)
            {
                snapshot.files.emplace_back(std::move(file));
            }
            else
            {
                snapshot.filesTruncated = true;
            }
        }

        void _countTrackedChange(RepoStatusSnapshot& snapshot, const RepoFileChange& file)
        {
            if (file.indexStatus != '.')
            {
                ++snapshot.stagedCount;
            }
            if (file.worktreeStatus != '.')
            {
                ++snapshot.modifiedCount;
            }
        }

        bool _parseHeader(const std::string_view record, RepoStatusSnapshot& snapshot)
        {
            constexpr std::string_view oidPrefix{ "# branch.oid " };
            constexpr std::string_view headPrefix{ "# branch.head " };
            constexpr std::string_view upstreamPrefix{ "# branch.upstream " };
            constexpr std::string_view aheadBehindPrefix{ "# branch.ab +" };

            if (record.starts_with(oidPrefix))
            {
                const auto value = record.substr(oidPrefix.size());
                if (value.empty())
                {
                    return false;
                }
                if (value == "(initial)")
                {
                    snapshot.unborn = true;
                    snapshot.headOid.clear();
                }
                else
                {
                    snapshot.headOid = value;
                }
            }
            else if (record.starts_with(headPrefix))
            {
                const auto value = record.substr(headPrefix.size());
                if (value.empty())
                {
                    return false;
                }
                if (value == "(detached)")
                {
                    snapshot.detached = true;
                    snapshot.branch.reset();
                }
                else
                {
                    snapshot.branch = value;
                }
            }
            else if (record.starts_with(upstreamPrefix))
            {
                const auto value = record.substr(upstreamPrefix.size());
                if (value.empty())
                {
                    return false;
                }
                snapshot.upstream = value;
            }
            else if (record.starts_with(aheadBehindPrefix))
            {
                const auto value = record.substr(aheadBehindPrefix.size());
                const auto separator = value.find(" -");
                if (separator == std::string_view::npos ||
                    !_parseCount(value.substr(0, separator), snapshot.ahead) ||
                    !_parseCount(value.substr(separator + 2), snapshot.behind))
                {
                    return false;
                }
            }

            return true;
        }

        bool _parseTrackedRecord(const std::string_view record, RepoFileChange& file)
        {
            auto remaining = record.substr(2);
            std::string_view xy;
            std::string_view submodule;
            std::string_view ignored;
            std::string_view path;

            const auto kind = record.front();
            const auto metadataFields = kind == '1' ? 8u : 9u;
            if (!_nextField(remaining, xy, metadataFields) ||
                !_nextField(remaining, submodule, metadataFields - 1) ||
                xy.size() != 2 ||
                submodule.size() != 4)
            {
                return false;
            }

            for (auto fieldsLeft = metadataFields - 2; fieldsLeft > 1; --fieldsLeft)
            {
                if (!_nextField(remaining, ignored, fieldsLeft))
                {
                    return false;
                }
            }
            if (!_nextField(remaining, path) || path.empty())
            {
                return false;
            }

            file.indexStatus = xy[0];
            file.worktreeStatus = xy[1];
            file.submodule = submodule[0] == 'S';
            file.path.assign(path);
            return true;
        }

        bool _parseUnmergedRecord(const std::string_view record, RepoFileChange& file)
        {
            auto remaining = record.substr(2);
            std::string_view xy;
            std::string_view submodule;
            std::string_view ignored;
            std::string_view path;
            constexpr size_t metadataFields = 10;

            if (!_nextField(remaining, xy, metadataFields) ||
                !_nextField(remaining, submodule, metadataFields - 1) ||
                xy.size() != 2 ||
                submodule.size() != 4)
            {
                return false;
            }

            for (auto fieldsLeft = metadataFields - 2; fieldsLeft > 1; --fieldsLeft)
            {
                if (!_nextField(remaining, ignored, fieldsLeft))
                {
                    return false;
                }
            }
            if (!_nextField(remaining, path) || path.empty())
            {
                return false;
            }

            file.indexStatus = xy[0];
            file.worktreeStatus = xy[1];
            file.submodule = submodule[0] == 'S';
            file.conflicted = true;
            file.path.assign(path);
            return true;
        }
    }

    RepoStatusParseResult ParseRepoStatus(const std::string_view output, const size_t maxRetainedFiles)
    {
        RepoStatusParseResult result;
        size_t offset = 0;

        while (offset < output.size())
        {
            const auto terminator = output.find('\0', offset);
            if (terminator == std::string_view::npos)
            {
                result.error = RepoStatusParseError::MissingRecordTerminator;
                return result;
            }

            const auto record = output.substr(offset, terminator - offset);
            offset = terminator + 1;
            if (record.empty())
            {
                result.error = RepoStatusParseError::MalformedRecord;
                return result;
            }

            if (record.front() == '#')
            {
                if (!_parseHeader(record, result.snapshot))
                {
                    result.error = RepoStatusParseError::MalformedHeader;
                    return result;
                }
                continue;
            }

            RepoFileChange file;
            switch (record.front())
            {
            case '1':
                if (!_parseTrackedRecord(record, file))
                {
                    result.error = RepoStatusParseError::MalformedRecord;
                    return result;
                }
                _countTrackedChange(result.snapshot, file);
                break;
            case '2':
                if (!_parseTrackedRecord(record, file))
                {
                    result.error = RepoStatusParseError::MalformedRecord;
                    return result;
                }
                _countTrackedChange(result.snapshot, file);
                if (offset >= output.size())
                {
                    result.error = RepoStatusParseError::MissingRenameSource;
                    return result;
                }
                {
                    const auto sourceTerminator = output.find('\0', offset);
                    if (sourceTerminator == std::string_view::npos || sourceTerminator == offset)
                    {
                        result.error = RepoStatusParseError::MissingRenameSource;
                        return result;
                    }
                    file.originalPath = std::string{ output.substr(offset, sourceTerminator - offset) };
                    offset = sourceTerminator + 1;
                }
                break;
            case 'u':
                if (!_parseUnmergedRecord(record, file))
                {
                    result.error = RepoStatusParseError::MalformedRecord;
                    return result;
                }
                ++result.snapshot.conflictedCount;
                break;
            case '?':
                if (record.size() <= 2 || record[1] != ' ')
                {
                    result.error = RepoStatusParseError::MalformedRecord;
                    return result;
                }
                file.path.assign(record.substr(2));
                file.untracked = true;
                ++result.snapshot.untrackedCount;
                break;
            case '!':
                continue;
            default:
                result.error = RepoStatusParseError::MalformedRecord;
                return result;
            }

            _retainFile(result.snapshot, std::move(file), maxRetainedFiles);
        }

        return result;
    }
}
