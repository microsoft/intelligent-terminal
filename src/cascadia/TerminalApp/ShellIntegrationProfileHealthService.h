// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ShellIntegrationProfileHealthService.h
//
// Process-wide scheduler and cache for read-only shell profile health checks.

#pragma once

#include <chrono>
#include <functional>
#include <mutex>
#include <optional>
#include <string_view>
#include <unordered_map>
#include <vector>

#include "../inc/ShellIntegrationProfileHealth.h"
#include "../inc/cppwinrt_utils.h"

namespace winrt::TerminalApp::implementation
{
    class ShellIntegrationProfileHealthService final
    {
    public:
        using Analyzer = std::function<::Microsoft::Terminal::ShellIntegration::Health::AnalysisResult(
            const ::Microsoft::Terminal::ShellIntegration::Health::TargetKey&,
            std::string_view)>;
        using Callback = std::function<void(
            const ::Microsoft::Terminal::ShellIntegration::Health::Result&)>;

        static ShellIntegrationProfileHealthService& Instance();

        uint64_t SetEnabled(bool enabled) noexcept;
        [[nodiscard]] uint64_t Generation() const noexcept;
        [[nodiscard]] bool Enabled() const noexcept;

        void Request(
            ::Microsoft::Terminal::ShellIntegration::Health::TargetKey target,
            uint64_t generation,
            Analyzer analyzer,
            Callback callback,
            bool force = false);

    private:
        struct TargetKeyHash
        {
            size_t operator()(const ::Microsoft::Terminal::ShellIntegration::Health::TargetKey& key) const noexcept;
        };

        struct PendingCallback
        {
            uint64_t generation;
            Callback callback;
        };

        struct Entry
        {
            std::optional<::Microsoft::Terminal::ShellIntegration::Health::Result> result;
            std::vector<PendingCallback> callbacks;
            std::optional<Analyzer> rerunAnalyzer;
            std::vector<PendingCallback> rerunCallbacks;
            std::chrono::steady_clock::time_point lastAttempt{};
            uint64_t activeRunToken{ 0 };
            bool inFlight{ false };
        };

        safe_void_coroutine _Run(
            ::Microsoft::Terminal::ShellIntegration::Health::TargetKey target,
            uint64_t generation,
            uint64_t runToken,
            Analyzer analyzer);

        mutable std::mutex _mutex;
        std::unordered_map<
            ::Microsoft::Terminal::ShellIntegration::Health::TargetKey,
            Entry,
            TargetKeyHash>
            _entries;
        uint64_t _generation{ 1 };
        uint64_t _nextRunToken{ 0 };
        bool _enabled{ false };
    };
}
