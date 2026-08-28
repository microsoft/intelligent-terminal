// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "BashProfileAnalyzer.h"

#include <cstring>
#include <limits>
#include <memory>

#include "../inc/BashShellIntegration.h"
#include "vendor/tree-sitter/core/include/tree_sitter/api.h"

extern "C" const TSLanguage* tree_sitter_bash(void);

using namespace Microsoft::Terminal::ShellIntegration;
using namespace Microsoft::Terminal::ShellIntegration::Health;

namespace
{
    struct ParserDeleter
    {
        void operator()(TSParser* parser) const noexcept
        {
            if (parser)
            {
                ts_parser_delete(parser);
            }
        }
    };

    struct TreeDeleter
    {
        void operator()(TSTree* tree) const noexcept
        {
            if (tree)
            {
                ts_tree_delete(tree);
            }
        }
    };

    [[nodiscard]] AnalysisResult _Result(const Status status, const Reason reason = Reason::None, const size_t blockStart = std::string::npos, const size_t blockEnd = std::string::npos) noexcept
    {
        return { status, reason, blockStart, blockEnd };
    }

    [[nodiscard]] bool _IsComment(const TSNode node) noexcept
    {
        return std::strcmp(ts_node_type(node), "comment") == 0;
    }

    [[nodiscard]] bool _IsExclusiveLine(const std::string_view profile, const size_t start, const size_t end) noexcept
    {
        const auto lineStart = profile.rfind('\n', start);
        const auto first = lineStart == std::string_view::npos ? 0 : lineStart + 1;
        const auto lineEnd = profile.find('\n', end);
        const auto last = lineEnd == std::string_view::npos ? profile.size() : lineEnd;
        const auto isHorizontalWhitespace = [](const char ch) noexcept {
            return ch == ' ' || ch == '\t' || ch == '\r';
        };

        for (auto i = first; i < start; ++i)
        {
            if (!isHorizontalWhitespace(profile[i]))
            {
                return false;
            }
        }
        for (auto i = end; i < last; ++i)
        {
            if (!isHorizontalWhitespace(profile[i]))
            {
                return false;
            }
        }
        return true;
    }

    [[nodiscard]] bool _StartsLine(const std::string_view profile, const size_t byteOffset) noexcept
    {
        return byteOffset == 0 || profile[byteOffset - 1] == '\n';
    }

    [[nodiscard]] std::string _CrLfCanonical(std::string_view lfCanonical)
    {
        std::string result;
        result.reserve(lfCanonical.size() * 2);
        for (const auto ch : lfCanonical)
        {
            if (ch == '\n')
            {
                result += "\r\n";
            }
            else
            {
                result += ch;
            }
        }
        return result;
    }

    struct MarkerInspection
    {
        size_t openCount{};
        size_t closeCount{};
        TSNode open{};
        TSNode close{};
        bool openIsRootChild{};
        bool closeIsRootChild{};
        bool malformedComment{};
        bool hasError{};
        bool exceededNodeLimit{};
    };

    void _InspectTree(const TSNode root, const std::string_view profile, const size_t maximumNodeCount, MarkerInspection& inspection) noexcept
    {
        TSTreeCursor cursor = ts_tree_cursor_new(root);
        size_t depth = 0;
        size_t visited = 0;

        while (true)
        {
            const auto node = ts_tree_cursor_current_node(&cursor);
            if (++visited > maximumNodeCount)
            {
                inspection.exceededNodeLimit = true;
                break;
            }

            if (ts_node_is_error(node) || ts_node_is_missing(node))
            {
                inspection.hasError = true;
            }

            if (_IsComment(node))
            {
                const auto start = static_cast<size_t>(ts_node_start_byte(node));
                const auto end = static_cast<size_t>(ts_node_end_byte(node));
                if (start > end || end > profile.size())
                {
                    inspection.hasError = true;
                }
                else
                {
                    const auto logicalEnd = end > start && profile[end - 1] == '\r' ? end - 1 : end;
                    const auto text = profile.substr(start, logicalEnd - start);
                    const auto hasOpen = text.find(kShellIntegrationBlockOpenMarker) != std::string_view::npos;
                    const auto hasClose = text.find(kShellIntegrationBlockCloseMarker) != std::string_view::npos;
                    if (hasOpen || hasClose)
                    {
                        if (text == kShellIntegrationBlockOpenMarker)
                        {
                            ++inspection.openCount;
                            inspection.open = node;
                            inspection.openIsRootChild = depth == 1;
                        }
                        else if (text == kShellIntegrationBlockCloseMarker)
                        {
                            ++inspection.closeCount;
                            inspection.close = node;
                            inspection.closeIsRootChild = depth == 1;
                        }
                        else
                        {
                            inspection.malformedComment = true;
                        }
                    }
                }
            }

            if (ts_tree_cursor_goto_first_child(&cursor))
            {
                ++depth;
                continue;
            }

            while (!ts_tree_cursor_goto_next_sibling(&cursor))
            {
                if (!ts_tree_cursor_goto_parent(&cursor))
                {
                    ts_tree_cursor_delete(&cursor);
                    return;
                }
                --depth;
            }
        }

        ts_tree_cursor_delete(&cursor);
    }

    [[nodiscard]] bool _HasExecutableRootChildAfter(const TSNode root, const size_t byteOffset) noexcept
    {
        const auto count = ts_node_child_count(root);
        for (uint32_t i = 0; i < count; ++i)
        {
            const auto child = ts_node_child(root, i);
            if (ts_node_start_byte(child) >= byteOffset &&
                ts_node_is_named(child) &&
                !_IsComment(child))
            {
                return true;
            }
        }
        return false;
    }
}

namespace Microsoft::Terminal::ShellIntegration::Bash::ProfileAnalyzer
{
    AnalysisResult Analyze(const std::string_view profileBytes) noexcept
    {
        try
        {
            if (profileBytes.size() > MaximumProfileBytes ||
                profileBytes.size() > static_cast<size_t>((std::numeric_limits<uint32_t>::max)()))
            {
                return _Result(Status::Indeterminate, Reason::FileTooLarge);
            }

            std::unique_ptr<TSParser, ParserDeleter> parser{ ts_parser_new() };
            if (!parser || !ts_parser_set_language(parser.get(), tree_sitter_bash()))
            {
                return _Result(Status::Indeterminate, Reason::ParseFailed);
            }

            ts_parser_set_timeout_micros(parser.get(), ParseTimeoutMicroseconds);
            std::unique_ptr<TSTree, TreeDeleter> tree{
                ts_parser_parse_string(parser.get(), nullptr, profileBytes.empty() ? "" : profileBytes.data(), static_cast<uint32_t>(profileBytes.size()))
            };
            if (!tree)
            {
                return _Result(Status::Indeterminate, Reason::TimedOut);
            }

            const auto root = ts_tree_root_node(tree.get());
            if (std::strcmp(ts_node_type(root), "program") != 0)
            {
                return _Result(Status::Indeterminate, Reason::ParseFailed);
            }

            MarkerInspection inspection;
            _InspectTree(root, profileBytes, MaximumNodeCount, inspection);
            if (inspection.exceededNodeLimit)
            {
                return _Result(Status::Indeterminate, Reason::TimedOut);
            }
            if (ts_node_has_error(root) || inspection.hasError)
            {
                return _Result(Status::Indeterminate, Reason::ParseFailed);
            }
            if (inspection.malformedComment ||
                inspection.openCount > 1 ||
                inspection.closeCount > 1 ||
                (inspection.openCount == 0) != (inspection.closeCount == 0))
            {
                return _Result(Status::Indeterminate, Reason::MalformedBlock);
            }
            if (inspection.openCount == 0)
            {
                return _Result(Status::NotInstalled, Reason::MissingBlock);
            }
            if (!inspection.openIsRootChild ||
                !inspection.closeIsRootChild)
            {
                return _Result(Status::Indeterminate, Reason::MalformedBlock);
            }

            const auto blockStart = static_cast<size_t>(ts_node_start_byte(inspection.open));
            auto blockEnd = static_cast<size_t>(ts_node_end_byte(inspection.close));
            if (blockEnd > blockStart && profileBytes[blockEnd - 1] == '\r')
            {
                --blockEnd;
            }
            if (blockStart >= blockEnd ||
                !_StartsLine(profileBytes, blockStart) ||
                !_IsExclusiveLine(profileBytes, blockStart, static_cast<size_t>(ts_node_end_byte(inspection.open))) ||
                !_IsExclusiveLine(profileBytes, static_cast<size_t>(ts_node_start_byte(inspection.close)), blockEnd))
            {
                return _Result(Status::Indeterminate, Reason::MalformedBlock);
            }

            const auto lfCanonical = Bash::BuildBlock("\n");
            const auto block = profileBytes.substr(blockStart, blockEnd - blockStart);
            if (block != lfCanonical)
            {
                const auto crLfCanonical = _CrLfCanonical(lfCanonical);
                if (block != crLfCanonical)
                {
                    return _Result(Status::Indeterminate, Reason::MalformedBlock);
                }
            }

            if (_HasExecutableRootChildAfter(root, blockEnd))
            {
                return _Result(Status::BlockNotLast, Reason::None, blockStart, blockEnd);
            }
            return _Result(Status::Healthy, Reason::None, blockStart, blockEnd);
        }
        catch (...)
        {
            return _Result(Status::Indeterminate, Reason::ParseFailed);
        }
    }
}
