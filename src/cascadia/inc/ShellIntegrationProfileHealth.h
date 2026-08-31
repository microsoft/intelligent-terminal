// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ShellIntegrationProfileHealth.h
//
// Shared data contract for the read-only shell-integration profile health
// pipeline. Shell-specific discovery and parsing live outside this header.

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

namespace Microsoft::Terminal::ShellIntegration::Health
{
    enum class ShellKind : uint8_t
    {
        PowerShell,
        WindowsPowerShell,
        GitBash,
        WslBash,
    };

    enum class ProfileSyntax : uint8_t
    {
        PowerShell,
        Bash,
    };

    enum class Status : uint8_t
    {
        Healthy,
        BlockNotLast,
        NotInstalled,
        Indeterminate,
    };

    enum class Reason : uint8_t
    {
        None,
        MissingBlock,
        MalformedBlock,
        ParseFailed,
        ReadFailed,
        FileTooLarge,
        ChangedDuringAnalysis,
        UnsupportedInvocation,
        UnsupportedHost,
        ShellIdentityUnavailable,
        TimedOut,
    };

    struct AnalysisResult
    {
        Status status{ Status::Indeterminate };
        Reason reason{ Reason::None };
        size_t blockStart{ std::string::npos };
        size_t blockEnd{ std::string::npos };

        [[nodiscard]] constexpr bool ShouldNotify() const noexcept
        {
            return status == Status::BlockNotLast;
        }

        [[nodiscard]] constexpr bool IsRetryable() const noexcept
        {
            return reason == Reason::ReadFailed ||
                   reason == Reason::ChangedDuringAnalysis ||
                   reason == Reason::ShellIdentityUnavailable ||
                   reason == Reason::TimedOut;
        }
    };

    // Stable identity for one profile-health target. `shellIdentity`
    // disambiguates instances that may share a display name, such as WSL
    // distributions. Window/profile/pane/session IDs intentionally do not
    // participate: multiple terminal profiles can load the same file.
    struct TargetKey
    {
        ShellKind shell{ ShellKind::PowerShell };
        ProfileSyntax syntax{ ProfileSyntax::PowerShell };
        std::wstring profilePath;
        std::wstring hostPath;
        std::wstring shellIdentity;

        [[nodiscard]] bool operator==(const TargetKey&) const noexcept = default;
    };

    struct ProfileFingerprint
    {
        bool exists{ false };
        uint64_t size{ 0 };
        uint64_t lastWriteTime{ 0 };
        uint64_t volumeSerialNumber{ 0 };
        uint64_t fileIndex{ 0 };
        uint64_t contentHash{ 0 };
        uint64_t mutationEpoch{ 0 };

        [[nodiscard]] bool operator==(const ProfileFingerprint&) const noexcept = default;
    };

    struct Result
    {
        TargetKey target;
        ProfileFingerprint fingerprint;
        AnalysisResult analysis;
    };
}
