// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once
#include "WslDistroName.h"

#include <optional>
#include <string>
#include <string_view>
#include <til/string.h>

namespace Microsoft::Terminal::Settings::Model
{
    enum class AgentPaneBackendSource
    {
        Host,
        Wsl,
    };

    struct AgentPaneBackend
    {
        AgentPaneBackendSource source{ AgentPaneBackendSource::Host };
        std::wstring agentId;
        std::wstring wslDistro;

        static std::optional<AgentPaneBackend> FromSettings(
            const std::wstring_view agentId,
            const std::wstring_view source)
        {
            if (agentId.empty())
            {
                return std::nullopt;
            }
            if (source.empty() || source == L"host")
            {
                return AgentPaneBackend{ AgentPaneBackendSource::Host, std::wstring{ agentId }, {} };
            }
            if (source.starts_with(L"wsl:") &&
                ::Microsoft::Terminal::WslDistroName::IsSafe(source.substr(4)) &&
                !agentId.starts_with(L"custom:"))
            {
                return AgentPaneBackend{ AgentPaneBackendSource::Wsl, std::wstring{ agentId }, std::wstring{ source.substr(4) } };
            }
            return std::nullopt;
        }

        static std::optional<AgentPaneBackend> FromSelection(const std::wstring_view selection)
        {
            return selection.starts_with(L"host:") || selection.starts_with(L"wsl:") ?
                       Parse(selection) :
                       FromSettings(selection, L"host");
        }

        std::wstring SettingsSource() const
        {
            return source == AgentPaneBackendSource::Wsl ? L"wsl:" + wslDistro : L"host";
        }

        static bool SourceEquals(const std::wstring_view lhs, const std::wstring_view rhs) noexcept
        {
            return lhs == rhs ||
                   (lhs.starts_with(L"wsl:") && rhs.starts_with(L"wsl:") &&
                    til::compare_ordinal_insensitive(lhs.substr(4), rhs.substr(4)) == 0);
        }

        static bool SelectionEquals(const std::wstring_view lhs, const std::wstring_view rhs)
        {
            if (lhs == rhs)
            {
                return true;
            }
            const auto left = Parse(lhs);
            const auto right = Parse(rhs);
            return left && right &&
                   left->agentId == right->agentId &&
                   SourceEquals(left->SettingsSource(), right->SettingsSource());
        }

        std::wstring SelectionId() const
        {
            return source == AgentPaneBackendSource::Wsl ? Wsl(wslDistro, agentId) : agentId;
        }

        static std::optional<AgentPaneBackend> Parse(const std::wstring_view value)
        {
            constexpr std::wstring_view hostPrefix{ L"host:" };
            constexpr std::wstring_view wslPrefix{ L"wsl:" };

            if (value.starts_with(hostPrefix))
            {
                const auto agentId = value.substr(hostPrefix.size());
                if (!agentId.empty())
                {
                    return AgentPaneBackend{
                        AgentPaneBackendSource::Host,
                        std::wstring{ agentId },
                        {},
                    };
                }
                return std::nullopt;
            }

            if (value.starts_with(wslPrefix))
            {
                const auto payload = value.substr(wslPrefix.size());
                const auto separator = payload.rfind(L':');
                if (separator != std::wstring_view::npos &&
                    separator > 0 &&
                    separator + 1 < payload.size() &&
                    ::Microsoft::Terminal::WslDistroName::IsSafe(payload.substr(0, separator)))
                {
                    return AgentPaneBackend{
                        AgentPaneBackendSource::Wsl,
                        std::wstring{ payload.substr(separator + 1) },
                        std::wstring{ payload.substr(0, separator) },
                    };
                }
            }

            return std::nullopt;
        }

        static std::wstring Host(const std::wstring_view agentId)
        {
            return L"host:" + std::wstring{ agentId };
        }

        static std::wstring Wsl(const std::wstring_view distro, const std::wstring_view agentId)
        {
            return L"wsl:" + std::wstring{ distro } + L":" + std::wstring{ agentId };
        }
    };
}
