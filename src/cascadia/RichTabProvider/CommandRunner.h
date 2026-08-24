// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "ProviderContracts.h"

#include <chrono>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace Microsoft::Terminal::RichTab::Provider
{
    struct CommandResult
    {
        enum class Status
        {
            InvalidRequest,
            ResolveFailed,
            LaunchFailed,
            InputWriteFailed,
            WaitFailed,
            TimedOut,
            OutputLimitExceeded,
            ExitCodeUnavailable,
            Completed,
        };

        Status status{ Status::LaunchFailed };
        uint32_t exitCode{ 0 };
        std::string standardOutput;
        std::string standardError;
        uint32_t win32Error{ 0 };
    };

    class CommandRunner
    {
    public:
        using Environment = std::vector<std::pair<std::wstring, std::wstring>>;

        struct LaunchInfo
        {
            std::filesystem::path executable;
            std::wstring commandLine;
        };

        static constexpr size_t MaximumRequestSize{ MaximumRequestPayloadSize };
        static constexpr size_t MaximumStandardOutputSize{ MaximumResponseSize };
        static constexpr size_t MaximumStandardErrorSize{ 64 * 1024 };

        CommandResult Run(
            const Manifest& manifest,
            std::string_view request,
            std::chrono::milliseconds timeout,
            const Environment& environment = {}) const;

        static std::optional<std::filesystem::path> ResolvePowerShell();
        static bool ResolveEntrypoint(
            const Manifest& manifest,
            std::filesystem::path& resolved,
            uint32_t& error) noexcept;
        static bool BuildLaunchInfo(
            const Manifest& manifest,
            LaunchInfo& launch,
            uint32_t& error);
        static std::wstring BuildEnvironment(const Environment& extraEnvironment);
        static void QuoteArgument(std::wstring_view argument, std::wstring& commandLine);
    };
}
