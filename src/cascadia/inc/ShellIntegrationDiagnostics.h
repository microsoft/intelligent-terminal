// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ShellIntegrationDiagnostics.h
//
// Pure helpers for parsing the normalized shell-integration diagnostic
// sequences raised by TerminalApp and for mapping the bounded persisted repair
// reasons used by ApplicationState.

#pragma once

#include <array>
#include <optional>
#include <string_view>

namespace Microsoft::Terminal::ShellIntegration::Diagnostics
{
    enum class ShellTarget
    {
        Pwsh,
        WindowsPowerShell,
    };

    enum class RepairReason
    {
        PromptChanged,
        RestartRequired,
        BindFailed,
    };

    enum class SignalKind
    {
        Ready,
        Repair,
        Runtime,
    };

    enum class RuntimeOutcome
    {
        Rebound,
        RebindFailed,
    };

    struct ParsedSignal
    {
        SignalKind kind;
        ShellTarget target;
        std::optional<RepairReason> repairReason;
        std::optional<RuntimeOutcome> runtimeOutcome;
    };

    namespace details
    {
        [[nodiscard]] constexpr bool SplitExact4(std::string_view input, std::array<std::string_view, 4>& fields) noexcept
        {
            size_t start = 0;
            for (size_t i = 0; i < fields.size(); ++i)
            {
                const auto end = input.find(';', start);
                if (i + 1 == fields.size())
                {
                    if (end != std::string_view::npos)
                    {
                        return false;
                    }
                    fields[i] = input.substr(start);
                    return true;
                }

                if (end == std::string_view::npos)
                {
                    return false;
                }

                fields[i] = input.substr(start, end - start);
                start = end + 1;
            }

            return false;
        }

        [[nodiscard]] constexpr std::optional<ShellTarget> ParseTarget(std::string_view token) noexcept
        {
            if (token == "pwsh")
            {
                return ShellTarget::Pwsh;
            }
            if (token == "powershell")
            {
                return ShellTarget::WindowsPowerShell;
            }
            return std::nullopt;
        }

        [[nodiscard]] constexpr std::optional<RepairReason> ParseRepairReason(std::string_view token) noexcept
        {
            if (token == "prompt-changed")
            {
                return RepairReason::PromptChanged;
            }
            if (token == "restart-required")
            {
                return RepairReason::RestartRequired;
            }
            if (token == "bind-failed")
            {
                return RepairReason::BindFailed;
            }
            return std::nullopt;
        }

        [[nodiscard]] constexpr std::optional<RuntimeOutcome> ParseRuntimeOutcome(std::string_view token) noexcept
        {
            if (token == "rebound")
            {
                return RuntimeOutcome::Rebound;
            }
            if (token == "rebind-failed")
            {
                return RuntimeOutcome::RebindFailed;
            }
            return std::nullopt;
        }
    }

    [[nodiscard]] constexpr std::optional<ParsedSignal> ParseSignal(std::string_view input) noexcept
    {
        std::array<std::string_view, 4> fields{};
        if (!details::SplitExact4(input, fields))
        {
            return std::nullopt;
        }

        if (fields[0] != "osc:9001")
        {
            return std::nullopt;
        }

        const auto target = details::ParseTarget(fields[2]);
        if (!target.has_value())
        {
            return std::nullopt;
        }

        if (fields[1] == "ShellIntegrationReady")
        {
            if (fields[3] != "7" && fields[3] != "8")
            {
                return std::nullopt;
            }

            return ParsedSignal{
                .kind = SignalKind::Ready,
                .target = *target,
                .repairReason = std::nullopt,
                .runtimeOutcome = std::nullopt,
            };
        }

        if (fields[1] == "ShellIntegrationRepair")
        {
            const auto reason = details::ParseRepairReason(fields[3]);
            if (!reason.has_value())
            {
                return std::nullopt;
            }

            return ParsedSignal{
                .kind = SignalKind::Repair,
                .target = *target,
                .repairReason = *reason,
                .runtimeOutcome = std::nullopt,
            };
        }

        if (fields[1] == "ShellIntegrationRuntime")
        {
            const auto outcome = details::ParseRuntimeOutcome(fields[3]);
            if (!outcome.has_value())
            {
                return std::nullopt;
            }

            return ParsedSignal{
                .kind = SignalKind::Runtime,
                .target = *target,
                .repairReason = std::nullopt,
                .runtimeOutcome = *outcome,
            };
        }

        return std::nullopt;
    }

    [[nodiscard]] constexpr std::wstring_view PersistedRepairReason(RepairReason reason) noexcept
    {
        switch (reason)
        {
        case RepairReason::PromptChanged:
            return L"prompt-changed";
        case RepairReason::RestartRequired:
            return L"restart-required";
        case RepairReason::BindFailed:
            return L"bind-failed";
        }

        return {};
    }

    [[nodiscard]] constexpr std::optional<RepairReason> ParsePersistedRepairReason(std::wstring_view reason) noexcept
    {
        if (reason == L"prompt-changed")
        {
            return RepairReason::PromptChanged;
        }
        if (reason == L"restart-required")
        {
            return RepairReason::RestartRequired;
        }
        if (reason == L"bind-failed")
        {
            return RepairReason::BindFailed;
        }
        return std::nullopt;
    }

    [[nodiscard]] constexpr std::wstring_view ShellLabel(ShellTarget target) noexcept
    {
        switch (target)
        {
        case ShellTarget::Pwsh:
            return L"PowerShell";
        case ShellTarget::WindowsPowerShell:
            return L"Windows PowerShell";
        }

        return {};
    }
}
