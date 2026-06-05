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
        // fold only — sufficient for matching shell-leaf strings in
        // commandlines (those are ASCII).
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

        // Returns true if the LAUNCH executable in `commandline` is
        // exactly `<leaf>` or `<leaf>.exe` (case-insensitive, leaf
        // compared after stripping the directory portion). The launch
        // exe is the first whitespace/quote-delimited token of the
        // commandline.
        //
        // Why "launch executable only" not "any token":
        //   * `pwsh -WorkingDirectory ~` — bare leaf, matches Pwsh ✓
        //   * `C:\Program Files\PowerShell\7\pwsh.exe -NoLogo` — leaf
        //     is pwsh.exe after path strip, matches Pwsh ✓
        //   * `cmd.exe /c echo pwsh` — launch exe is cmd.exe; the
        //     `pwsh` in the arg list MUST NOT match Pwsh (the user is
        //     running cmd.exe and just happens to print the string
        //     "pwsh"). Plain "any-token" matching would mis-classify
        //     this. Anchoring on the launch exe avoids that whole
        //     class of false positive.
        //   * `mypwsh.exe` — launch leaf is mypwsh.exe, not pwsh or
        //     pwsh.exe, no match ✓.
        inline bool _CommandlineHasExeToken(std::wstring_view commandline, std::wstring_view leaf) noexcept
        {
            if (leaf.empty())
            {
                return false;
            }
            // Skip leading whitespace.
            size_t start = 0;
            while (start < commandline.size() &&
                   (commandline[start] == L' ' || commandline[start] == L'\t'))
            {
                ++start;
            }
            // Honor an opening quote — the launch exe path may contain
            // spaces (e.g. "C:\Program Files\...\pwsh.exe").
            bool quoted = false;
            if (start < commandline.size() && commandline[start] == L'"')
            {
                quoted = true;
                ++start;
            }
            // First token ends at the matching close-quote OR
            // whitespace OR end-of-string.
            size_t end = start;
            while (end < commandline.size())
            {
                const wchar_t c = commandline[end];
                if (quoted ? (c == L'"') : (c == L' ' || c == L'\t'))
                {
                    break;
                }
                ++end;
            }
            if (end <= start)
            {
                return false;
            }
            // Strip directory portion of the launch exe: leaf is the
            // substring after the last `\` or `/`.
            size_t leafStart = start;
            for (size_t i = start; i < end; ++i)
            {
                if (commandline[i] == L'\\' || commandline[i] == L'/')
                {
                    leafStart = i + 1;
                }
            }
            const std::wstring_view leafToken = commandline.substr(leafStart, end - leafStart);
            const auto fold = [](wchar_t c) noexcept -> wchar_t {
                return (c >= L'A' && c <= L'Z') ? static_cast<wchar_t>(c + (L'a' - L'A')) : c;
            };
            const auto equalsCi = [&](std::wstring_view a, std::wstring_view b) noexcept -> bool {
                if (a.size() != b.size()) return false;
                for (size_t i = 0; i < a.size(); ++i)
                {
                    if (fold(a[i]) != fold(b[i])) return false;
                }
                return true;
            };
            if (equalsCi(leafToken, leaf))
            {
                return true; // bare-leaf form
            }
            // Try with .exe suffix.
            if (leafToken.size() == leaf.size() + 4)
            {
                constexpr std::wstring_view dotExe{ L".exe" };
                bool exeMatch = true;
                for (size_t i = 0; i < leaf.size(); ++i)
                {
                    if (fold(leafToken[i]) != fold(leaf[i])) { exeMatch = false; break; }
                }
                if (exeMatch)
                {
                    for (size_t i = 0; i < dotExe.size(); ++i)
                    {
                        if (fold(leafToken[leaf.size() + i]) != fold(dotExe[i])) { exeMatch = false; break; }
                    }
                    if (exeMatch) return true; // <leaf>.exe form
                }
            }
            return false;
        }
    }

    // Returns true if the given (source, commandline) pair represents a
    // profile that uses `target`.
    //
    // Matching strategy (intentionally simple — token-bounded leaf
    // match + one source discriminator — to avoid over-engineering this
    // gate while still recognizing both the path-with-.exe and bare
    // forms a user may legitimately set in their profile commandline):
    //
    //   * Pwsh: source == "Windows.Terminal.PowershellCore" OR
    //           commandline contains the leaf token "pwsh"
    //           (matches both `pwsh.exe` and bare `pwsh`).
    //   * WindowsPowerShell: commandline contains leaf token
    //           "powershell" AND does NOT contain leaf token
    //           "pwsh". pwsh.exe installs under
    //           "...\\PowerShell\\7\\pwsh.exe" so a bare "powershell"
    //           token match would mis-classify it — the NOT-pwsh
    //           discriminator distinguishes the two leaf .exes.
    //   * Bash (Git Bash): commandline contains leaf token "bash"
    //           AND does NOT contain leaf token "wsl". WSL distro
    //           profiles use "wsl.exe -d <distro>" (or "wsl -d ...")
    //           and must NOT be matched as Git Bash (they're covered
    //           by the Wsl-source iteration on the caller side).
    //
    // Commandline matching is case-insensitive (token-bounded leaf).
    // The source-string check is case-SENSITIVE because the WT
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
            return details::_CommandlineHasExeToken(commandline, L"pwsh");
        case Target::WindowsPowerShell:
            return details::_CommandlineHasExeToken(commandline, L"powershell") &&
                   !details::_CommandlineHasExeToken(commandline, L"pwsh");
        case Target::Bash:
            return details::_CommandlineHasExeToken(commandline, L"bash") &&
                   !details::_CommandlineHasExeToken(commandline, L"wsl");
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
