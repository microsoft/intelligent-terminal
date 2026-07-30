// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <optional>
#include <string>
#include <string_view>

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
                    separator + 1 < payload.size())
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
