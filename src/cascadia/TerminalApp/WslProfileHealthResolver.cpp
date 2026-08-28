// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "WslProfileHealthResolver.h"

#include <utility>

#include "../inc/WslShellIntegration.h"

namespace Microsoft::Terminal::ShellIntegration::Wsl::ProfileHealth
{
    namespace
    {
        std::optional<Health::TargetKey> _CreateTarget(
            const std::wstring_view distroName,
            std::wstring profilePath) noexcept
        {
            if (!details::IsSafeDistroName(distroName) || profilePath.empty())
            {
                return std::nullopt;
            }

            try
            {
                Health::TargetKey target;
                target.shell = Health::ShellKind::WslBash;
                target.syntax = Health::ProfileSyntax::Bash;
                target.profilePath = std::move(profilePath);
                // The ShellKind already identifies WSL bash. Prefixing the
                // validated distro name makes the identity self-describing
                // without including transient pane or session state.
                target.shellIdentity = L"wsl:";
                target.shellIdentity.append(distroName);
                return target;
            }
            catch (...)
            {
                return std::nullopt;
            }
        }

        bool _IsExplicitWslBash(const Invocation::Classification& classification) noexcept
        {
            // WslIdentityRequired is expected for an explicit bash command
            // selecting the default distro or a --distribution-id. It is safe
            // to resolve that identity here. Do not similarly admit the
            // legacy bash.exe path or a WSL default guest: neither proves
            // bash is the actual shell.
            return classification.shellKind.has_value() &&
                   *classification.shellKind == Health::ShellKind::WslBash &&
                   classification.wsl.launcher == Invocation::WslLauncher::WslExe &&
                   classification.wsl.guestShell == Invocation::WslGuestShell::Bash &&
                   !classification.wsl.requiresDefaultShellConfirmation &&
                   (classification.eligible ||
                    classification.reason == Invocation::Reason::WslIdentityRequired);
        }
    }

    std::optional<Health::TargetKey> TryCreateTargetFromVerifiedIdentity(
        const std::wstring_view distroName,
        const std::string_view home) noexcept
    {
        if (!details::IsSafeDistroName(distroName) || !details::IsSafeHome(home))
        {
            return std::nullopt;
        }

        try
        {
            return _CreateTarget(distroName, UncPath(distroName, std::string{ home } + "/.bashrc"));
        }
        catch (...)
        {
            return std::nullopt;
        }
    }

    Resolution ResolveTarget(
        const std::wstring_view effectiveWslCommandline,
        const Invocation::Classification& classification) noexcept
    {
        if (!_IsExplicitWslBash(classification) || effectiveWslCommandline.empty())
        {
            return { std::nullopt, ResolutionFailure::NotExplicitBash };
        }

        try
        {
            // This peek is free and avoids another probe when installation or
            // another health request has already resolved the same command.
            // It intentionally cannot produce a target alone because the
            // cached public label has no HOME value.
            const auto cachedDistroName = ProbedDistroName(effectiveWslCommandline);

            // WslBashFlavor reuses the existing identity cache and its one
            // bounded probe. It constructs its profile path only after the
            // reported distro identity and HOME pass the shared validators.
            WslBashFlavor flavor{ std::wstring{ effectiveWslCommandline } };
            if (!flavor.Valid())
            {
                return { std::nullopt, ResolutionFailure::IdentityUnavailable };
            }

            const auto distroName = cachedDistroName.empty() ?
                                        ProbedDistroName(effectiveWslCommandline) :
                                        cachedDistroName;
            const auto target = _CreateTarget(distroName, flavor.ProfilePath());
            if (!target)
            {
                return { std::nullopt, ResolutionFailure::IdentityUnavailable };
            }
            return { target, ResolutionFailure::None };
        }
        catch (...)
        {
            return { std::nullopt, ResolutionFailure::IdentityUnavailable };
        }
    }
}
