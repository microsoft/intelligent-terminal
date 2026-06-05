// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ShellIntegrationProfileGate.h
//
// Profile-presence gate for shell-integration install/reconcile.
//
// Rationale: a user may keep ONLY a "Developer PowerShell for VS"
// profile (which uses Windows PowerShell) and delete the default
// Windows PowerShell profile; or use only pwsh and have no Windows
// PowerShell profile; or not have Git Bash installed at all. Installing
// shell integration for shells the user has no profile for writes a
// file the user will never source — pure noise that pollutes their
// HOME and the diff in their version-controlled dotfiles.
//
// This header exposes two pure functions:
//
//   * _ProfileMatchesShell(target, source, commandline) — pure,
//     trivially unit-testable. Substring-based shell detection rules
//     documented at the function.
//
//   * AnyProfileUsesShell<ProfilesT>(target, profiles) — template
//     iterator that calls _ProfileMatchesShell on every profile in
//     the collection. Catches and ignores any per-profile exception
//     (a profile whose Source() or Commandline() throws simply does
//     not contribute to the result; it never tanks the whole gate).
//
// Note: WSL distros are NOT covered here. The caller already iterates
// `_settings.AllProfiles()` filtering on `Source=="Windows.Terminal.Wsl"`
// and emits one Install call per matched profile — that path is
// already profile-gated by construction.

#pragma once

#include <string_view>

#include "ShellIntegrationCommon.h"

namespace Microsoft::Terminal::ShellIntegration
{
    namespace details
    {
        // Case-insensitive substring match on UTF-16 code units. ASCII
        // fold only — sufficient for matching "pwsh.exe" / "powershell.exe"
        // / "bash.exe" / "wsl.exe" in commandlines (those are ASCII).
        inline bool _CaseInsensitiveContains(std::wstring_view haystack, std::wstring_view needle) noexcept
        {
            if (needle.empty())
            {
                return true;
            }
            if (haystack.size() < needle.size())
            {
                return false;
            }
            const auto fold = [](wchar_t c) noexcept -> wchar_t {
                return (c >= L'A' && c <= L'Z') ? static_cast<wchar_t>(c + (L'a' - L'A')) : c;
            };
            const auto limit = haystack.size() - needle.size();
            for (size_t i = 0; i <= limit; ++i)
            {
                bool match = true;
                for (size_t j = 0; j < needle.size(); ++j)
                {
                    if (fold(haystack[i + j]) != fold(needle[j]))
                    {
                        match = false;
                        break;
                    }
                }
                if (match)
                {
                    return true;
                }
            }
            return false;
        }
    }

    // Returns true if the given (source, commandline) pair represents a
    // profile that uses `target`.
    //
    // Matching strategy (intentionally simple — substring + one source
    // discriminator — to avoid over-engineering this gate):
    //
    //   * Pwsh: source == "Windows.Terminal.PowershellCore" OR
    //           commandline contains "pwsh.exe".
    //   * WindowsPowerShell: commandline contains "powershell.exe" AND
    //           does NOT contain "pwsh.exe". pwsh.exe installs under
    //           "...\\PowerShell\\7\\pwsh.exe" so a bare "powershell"
    //           substring match would mis-classify it — anchor on the
    //           leaf .exe instead.
    //   * Bash (Git Bash): commandline contains "bash.exe" AND does
    //           NOT contain "wsl.exe". WSL distro profiles use
    //           "wsl.exe -d <distro>" or `wsl.exe ~ -d <distro>` and
    //           must NOT be matched as Git Bash (they're covered by
    //           the Wsl-source iteration on the caller side).
    //
    // Commandline matching is case-insensitive (substring on the leaf
    // .exe). The source-string check is case-SENSITIVE because the WT
    // dynamic-profile generators emit `Source` values with a fixed
    // canonical case (e.g. exactly "Windows.Terminal.PowershellCore");
    // see LegacyProfileGeneratorNamespaces.h. A case-insensitive source
    // match would be unnecessary work.
    // Returns false for any other target (e.g. a hypothetical future
    // shell flavor) — caller is responsible for adding a new branch
    // when registering a new Target.
    inline bool _ProfileMatchesShell(Target target,
                                     std::wstring_view source,
                                     std::wstring_view commandline) noexcept
    {
        switch (target)
        {
        case Target::Pwsh:
            if (source == L"Windows.Terminal.PowershellCore")
            {
                return true;
            }
            return details::_CaseInsensitiveContains(commandline, L"pwsh.exe");
        case Target::WindowsPowerShell:
            return details::_CaseInsensitiveContains(commandline, L"powershell.exe") &&
                   !details::_CaseInsensitiveContains(commandline, L"pwsh.exe");
        case Target::Bash:
            return details::_CaseInsensitiveContains(commandline, L"bash.exe") &&
                   !details::_CaseInsensitiveContains(commandline, L"wsl.exe");
        default:
            return false;
        }
    }

    // Iterates the profile collection and returns true if any profile
    // matches `target`. A per-profile exception (e.g. Source() or
    // Commandline() throws) is swallowed for that one profile — it
    // simply doesn't contribute. The function never throws.
    //
    // Templated so it works with both winrt::Windows::Foundation::Collections::IVectorView<Model::Profile>
    // (the live `_settings.AllProfiles()` view) and any std::vector-like
    // collection of test doubles that expose .Source() / .Commandline().
    template<typename ProfilesT>
    inline bool AnyProfileUsesShell(Target target, const ProfilesT& profiles) noexcept
    {
        try
        {
            for (const auto& profile : profiles)
            {
                try
                {
                    const auto src = profile.Source();
                    const auto cmd = profile.Commandline();
                    if (_ProfileMatchesShell(target,
                                             std::wstring_view{ src },
                                             std::wstring_view{ cmd }))
                    {
                        return true;
                    }
                }
                catch (...)
                {
                    // One bad profile must not tank the whole gate.
                }
            }
        }
        catch (...)
        {
            // Iteration itself raced with a settings reload, or the
            // collection is in a bad state. Fail closed (return false):
            // installing for a shell the user might not have is the
            // exact bug this gate exists to prevent.
        }
        return false;
    }
}
