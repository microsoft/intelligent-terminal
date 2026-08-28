// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ShellIntegrationInvocation.h
//
// Fail-closed classification of an already-effective shell launch command
// line. This does not perform file I/O, resolve executables, inspect settings,
// resolve WSL identities, or start a process. It does query the Windows
// directory through GetWindowsDirectoryW to recognize OS-owned launchers.

#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>

#include "../inc/ShellIntegrationProfileHealth.h"

namespace Microsoft::Terminal::ShellIntegration::Invocation
{
    enum class Reason : uint8_t
    {
        Eligible,
        EmptyCommandline,
        CommandlineParseFailed,
        RootNotAbsolute,
        InvalidActualRoot,
        ActualRootLeafMismatch,
        ActualRootPathMismatch,
        UnsupportedRoot,

        PowerShellUnsupportedOption,
        PowerShellNoProfile,
        PowerShellNonInteractive,
        PowerShellFile,
        PowerShellCommand,
        PowerShellEncodedCommand,

        BashUnsupportedOption,
        BashNoRc,
        BashCustomRcFile,
        BashCommand,
        BashScriptOperand,
        BashLoginNoProfile,
        BashLoginProfileUnproven,
        BashPosixMode,

        WslUnsupportedHostOption,
        WslMalformedHostOption,
        WslMissingGuestCommand,
        WslIdentityRequired,
        WslDefaultShellConfirmationRequired,
        WslExplicitFish,
        WslExplicitZsh,
        WslUnsupportedGuest,
    };

    enum class WslLauncher : uint8_t
    {
        None,
        WslExe,
        LegacySystem32Bash,
    };

    enum class WslGuestShell : uint8_t
    {
        None,
        Default,
        Bash,
        Fish,
        Zsh,
        Other,
    };

    // Selection is retained as parsed input, rather than normalized or
    // resolved. In particular, a distribution ID is not an identity: only a
    // caller which has queried WSL may supply the resulting stable identity.
    struct WslInvocation
    {
        WslLauncher launcher{ WslLauncher::None };
        WslGuestShell guestShell{ WslGuestShell::None };
        std::optional<std::wstring> distribution;
        std::optional<std::wstring> distributionId;
        bool usesExec{ false };
        bool requiresIdentity{ false };
        bool requiresDefaultShellConfirmation{ false };
    };

    struct Classification
    {
        bool eligible{ false };
        std::optional<Health::ShellKind> shellKind;
        Reason reason{ Reason::EmptyCommandline };
        WslInvocation wsl;
    };

    // `actualRootImagePath`, when supplied, is the root process image path
    // obtained by the caller. It is used only as supplied data; no path is
    // opened or resolved here. A bare command root is accepted only when this
    // argument is an absolute image whose leaf agrees with argv[0]. Relative
    // qualified roots (for example, ".\\pwsh.exe") are always rejected. When
    // both roots are absolute, their case/slash-normalized full paths must
    // agree, not merely their leaves.
    //
    // A non-System32 bash.exe is classified as the GitBash-compatible target,
    // matching ShellIntegrationProfileGate. This function cannot determine an
    // MSYS2/custom HOME mapping; callers must not infer one from this result.
    // Bash's implicit-interactive result additionally assumes the caller has
    // already established a direct ConPTY host with its TTY attached.
    [[nodiscard]] Classification Classify(
        std::wstring_view effectiveCommandline,
        std::optional<std::wstring_view> actualRootImagePath = std::nullopt) noexcept;
}
