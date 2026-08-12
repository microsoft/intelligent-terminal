// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "CompletionProvider.h"
#include "PromptInputModel.h"

namespace winrt::TerminalApp::implementation
{
    class PathCompletionProvider
    {
    public:
        std::vector<CompletionProviderResult> Query(const PromptInputSnapshot& input,
                                                    std::wstring_view workingDirectory);

    private:
        struct DirectoryEntry
        {
            std::wstring name;
            bool directory{ false };
        };

        struct CacheEntry
        {
            std::vector<DirectoryEntry> entries;
            std::filesystem::file_time_type writeTime{};
            uint64_t access{ 0 };
        };

        std::vector<DirectoryEntry> _directoryEntries(const std::filesystem::path& directory);
        static bool _startsWithInsensitive(std::wstring_view value, std::wstring_view prefix) noexcept;
        static std::wstring _expandPath(std::wstring_view path);

        std::mutex _cacheMutex;
        std::unordered_map<std::wstring, CacheEntry> _cache;
        uint64_t _accessCounter{ 0 };
    };
}
