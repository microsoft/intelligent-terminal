// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "PathCompletionProvider.h"

#include <unordered_set>

namespace winrt::TerminalApp::implementation
{
    namespace
    {
        constexpr size_t MaxResults = 100;
        constexpr size_t MaxCachedDirectories = 128;

        std::wstring _lowercase(std::wstring value)
        {
            std::transform(value.begin(), value.end(), value.begin(), towlower);
            return value;
        }

        std::unordered_set<std::wstring> _executableExtensions()
        {
            std::unordered_set<std::wstring> extensions;
            wchar_t* pathExtValue = nullptr;
            size_t ignored = 0;
            if (_wdupenv_s(&pathExtValue, &ignored, L"PATHEXT") == 0 && pathExtValue)
            {
                const std::unique_ptr<wchar_t, decltype(&free)> pathExtOwner{ pathExtValue, &free };
                std::wstring_view remaining{ pathExtValue };
                while (!remaining.empty())
                {
                    const auto separator = remaining.find(L';');
                    auto extension = std::wstring{ remaining.substr(0, separator) };
                    remaining = separator == std::wstring_view::npos ? std::wstring_view{} : remaining.substr(separator + 1);
                    if (!extension.empty())
                    {
                        extensions.emplace(_lowercase(std::move(extension)));
                    }
                }
            }
            if (extensions.empty())
            {
                extensions = { L".com", L".exe", L".bat", L".cmd" };
            }
            return extensions;
        }
    }

    std::vector<CompletionProviderResult> PathCompletionProvider::Query(const PromptInputSnapshot& input,
                                                                        const std::wstring_view workingDirectory)
    {
        std::vector<CompletionProviderResult> results;
        if (!input.trusted || input.cursor != input.text.size())
        {
            return results;
        }

        const auto beforeCursor = std::wstring_view{ input.text }.substr(0, input.cursor);
        size_t tokenStart = 0;
        wchar_t activeQuote = L'\0';
        for (size_t i = 0; i < beforeCursor.size(); ++i)
        {
            const auto character = beforeCursor[i];
            if (character == L'"' || character == L'\'')
            {
                activeQuote = activeQuote == character ? L'\0' : (activeQuote == L'\0' ? character : activeQuote);
            }
            else if (activeQuote == L'\0' && (character == L' ' || character == L'\t'))
            {
                tokenStart = i + 1;
            }
        }
        auto token = std::wstring{ beforeCursor.substr(tokenStart) };
        const auto quoted = !token.empty() && (token.front() == L'"' || token.front() == L'\'');
        const auto quote = quoted ? token.front() : L'\0';
        if (quoted)
        {
            token.erase(token.begin());
            ++tokenStart;
        }
        if (token.empty())
        {
            return results;
        }

        const auto firstToken = beforeCursor.substr(0, tokenStart).find_first_not_of(L" \t") == std::wstring_view::npos;
        const auto hasPathSyntax = token.find_first_of(L"\\/:%~") != std::wstring::npos || token.starts_with(L".");

        if (firstToken && !hasPathSyntax)
        {
            std::unordered_set<std::wstring> seen;
            const auto executableExtensions = _executableExtensions();
            wchar_t* pathValue = nullptr;
            size_t ignored = 0;
            if (_wdupenv_s(&pathValue, &ignored, L"PATH") == 0 && pathValue)
            {
                const std::unique_ptr<wchar_t, decltype(&free)> pathOwner{ pathValue, &free };
                std::wstring_view remaining{ pathValue };
                while (!remaining.empty())
                {
                    const auto separator = remaining.find(L';');
                    const auto directoryText = remaining.substr(0, separator);
                    remaining = separator == std::wstring_view::npos ? std::wstring_view{} : remaining.substr(separator + 1);
                    const auto directory = std::filesystem::path{ _expandPath(directoryText) };
                    for (const auto& entry : _directoryEntries(directory))
                    {
                        if (entry.directory || !_startsWithInsensitive(entry.name, token))
                        {
                            continue;
                        }
                        if (!executableExtensions.contains(_lowercase(std::filesystem::path{ entry.name }.extension().wstring())))
                        {
                            continue;
                        }
                        const auto key = _lowercase(entry.name);
                        if (!seen.emplace(key).second)
                        {
                            continue;
                        }
                        results.push_back(CompletionProviderResult{
                            .completionText = entry.name,
                            .displayText = entry.name,
                            .description = directory.wstring(),
                            .resultType = 2,
                            .replacementIndex = gsl::narrow_cast<uint32_t>(tokenStart),
                            .replacementLength = gsl::narrow_cast<uint32_t>(token.size()),
                            .cursorIndex = gsl::narrow_cast<uint32_t>(input.cursor),
                        });
                        if (results.size() >= MaxResults)
                        {
                            return results;
                        }
                    }
                }
            }
            return results;
        }

        const auto separator = token.find_last_of(L"\\/");
        const auto directoryPart = separator == std::wstring::npos ? std::wstring{} : token.substr(0, separator + 1);
        const auto prefix = separator == std::wstring::npos ? token : token.substr(separator + 1);
        auto lookupDirectory = std::filesystem::path{ _expandPath(directoryPart) };
        if (lookupDirectory.empty())
        {
            lookupDirectory = std::filesystem::path{ workingDirectory };
        }
        else if (lookupDirectory.is_relative())
        {
            lookupDirectory = std::filesystem::path{ workingDirectory } / lookupDirectory;
        }

        for (const auto& entry : _directoryEntries(lookupDirectory.lexically_normal()))
        {
            if (!_startsWithInsensitive(entry.name, prefix))
            {
                continue;
            }

            auto completion = directoryPart + entry.name;
            if (entry.directory)
            {
                completion.push_back(L'\\');
            }
            if (!quoted && completion.find(L' ') != std::wstring::npos)
            {
                completion = L"\"" + completion + (entry.directory ? L"" : L"\"");
            }
            else if (quoted)
            {
                completion.insert(completion.begin(), quote);
                if (!entry.directory)
                {
                    completion.push_back(quote);
                }
            }

            results.push_back(CompletionProviderResult{
                .completionText = std::move(completion),
                .displayText = entry.name,
                .description = lookupDirectory.wstring(),
                .resultType = entry.directory ? 4 : 3,
                .replacementIndex = gsl::narrow_cast<uint32_t>(tokenStart - (quoted ? 1 : 0)),
                .replacementLength = gsl::narrow_cast<uint32_t>(token.size() + (quoted ? 1 : 0)),
                .cursorIndex = gsl::narrow_cast<uint32_t>(input.cursor),
            });
            if (results.size() >= MaxResults)
            {
                break;
            }
        }
        return results;
    }

    std::vector<PathCompletionProvider::DirectoryEntry> PathCompletionProvider::_directoryEntries(const std::filesystem::path& directory)
    {
        std::error_code ec;
        const auto normalized = directory.lexically_normal().wstring();
        const auto writeTime = std::filesystem::last_write_time(directory, ec);
        if (ec)
        {
            return {};
        }

        {
            const std::scoped_lock lock{ _cacheMutex };
            if (const auto cached = _cache.find(normalized);
                cached != _cache.end() && cached->second.writeTime == writeTime)
            {
                cached->second.access = ++_accessCounter;
                return cached->second.entries;
            }
        }

        std::vector<DirectoryEntry> entries;
        for (std::filesystem::directory_iterator iterator{ directory, std::filesystem::directory_options::skip_permission_denied, ec };
             !ec && iterator != std::filesystem::directory_iterator{};
             iterator.increment(ec))
        {
            const auto name = iterator->path().filename().wstring();
            if (!name.empty())
            {
                entries.push_back(DirectoryEntry{ name, iterator->is_directory(ec) });
                ec.clear();
            }
        }
        std::sort(entries.begin(), entries.end(), [](const auto& lhs, const auto& rhs) {
            return _wcsicmp(lhs.name.c_str(), rhs.name.c_str()) < 0;
        });

        const std::scoped_lock lock{ _cacheMutex };
        _cache[normalized] = CacheEntry{ entries, writeTime, ++_accessCounter };
        if (_cache.size() > MaxCachedDirectories)
        {
            const auto oldest = std::min_element(_cache.begin(), _cache.end(), [](const auto& lhs, const auto& rhs) {
                return lhs.second.access < rhs.second.access;
            });
            if (oldest != _cache.end())
            {
                _cache.erase(oldest);
            }
        }
        return entries;
    }

    bool PathCompletionProvider::_startsWithInsensitive(const std::wstring_view value,
                                                        const std::wstring_view prefix) noexcept
    {
        return value.size() >= prefix.size() &&
               _wcsnicmp(value.data(), prefix.data(), prefix.size()) == 0;
    }

    std::wstring PathCompletionProvider::_expandPath(const std::wstring_view path)
    {
        std::wstring expanded{ path };
        if (expanded.starts_with(L"~"))
        {
            wchar_t* home = nullptr;
            size_t ignored = 0;
            if (_wdupenv_s(&home, &ignored, L"USERPROFILE") == 0 && home)
            {
                const std::unique_ptr<wchar_t, decltype(&free)> homeOwner{ home, &free };
                expanded.replace(0, 1, home);
            }
        }

        const auto required = ExpandEnvironmentStringsW(expanded.c_str(), nullptr, 0);
        if (required > 0)
        {
            std::wstring environmentExpanded(required, L'\0');
            ExpandEnvironmentStringsW(expanded.c_str(), environmentExpanded.data(), required);
            environmentExpanded.pop_back();
            return environmentExpanded;
        }
        return expanded;
    }
}
