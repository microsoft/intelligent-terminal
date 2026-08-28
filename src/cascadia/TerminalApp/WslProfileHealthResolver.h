// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// WslProfileHealthResolver.h
//
// Resolves the read-only profile-health target for an explicitly invoked
// WSL bash shell.

#pragma once

#include <cstdint>
#include <optional>
#include <string_view>

#include "../inc/ShellIntegrationProfileHealth.h"
#include "ShellIntegrationInvocation.h"

namespace Microsoft::Terminal::ShellIntegration::Wsl::ProfileHealth
{
    enum class ResolutionFailure : uint8_t
    {
        None,
        NotExplicitBash,
        IdentityUnavailable,
    };

    struct Resolution
    {
        std::optional<Health::TargetKey> target;
        ResolutionFailure failure{ ResolutionFailure::IdentityUnavailable };

        [[nodiscard]] explicit operator bool() const noexcept { return target.has_value(); }
    };

    // Builds a WSL bash profile-health target from an identity that has already
    // been obtained from the distro. This function does not access WSL or the
    // filesystem. It validates both fields again before constructing the UNC
    // profile path, so untrusted identity data fails closed.
    [[nodiscard]] std::optional<Health::TargetKey> TryCreateTargetFromVerifiedIdentity(
        std::wstring_view distroName,
        std::string_view home) noexcept;

    // Resolves a target for `effectiveWslCommandline` synchronously. This API
    // may block for up to the existing WSL identity probe's 30-second bound,
    // including while a stopped WSL2 distro cold-starts. It MUST be called
    // from a background worker, never the UI thread.
    //
    // `classification` must be the classification of this exact effective
    // command line. An explicit WSL bash guest is accepted when it is
    // otherwise eligible, or when WSL identity resolution is its only
    // outstanding requirement. Bare/default WSL remains fail-closed: this
    // resolver does not infer or confirm a distro's default shell. No command
    // line, path, or profile content is logged or emitted as telemetry.
    [[nodiscard]] Resolution ResolveTarget(
        std::wstring_view effectiveWslCommandline,
        const Invocation::Classification& classification) noexcept;
}
