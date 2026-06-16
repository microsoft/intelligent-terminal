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
//   * ProfileMatchesShell(target, source, commandline) — pure,
//     trivially unit-testable. Launch-exe leaf token matching with
//     a source discriminator for the Pwsh dynamic generator; see
//     the rules table at the function body.
//
//   * AnyProfileUsesShell<ProfilesT>(target, profiles) — template
//     iterator that calls ProfileMatchesShell on every profile in
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
#include <utility>
#include "ShellIntegrationCommon.h"

namespace Microsoft::Terminal::ShellIntegration
{
    namespace details
    {
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
        //   * `pwshell.exe` — launch leaf is pwshell.exe, not pwsh or
        //     pwsh.exe, no match ✓.
        inline wchar_t FoldAsciiLower(wchar_t c) noexcept
        {
            return (c >= L'A' && c <= L'Z') ? static_cast<wchar_t>(c + (L'a' - L'A')) : c;
        }

        // True if `commandline` at `pos` begins at a filesystem root: a
        // drive path (`X:\` or `X:/`) or a UNC path (`\\`). Only such an
        // unquoted launch executable can legitimately contain spaces
        // (e.g. `C:\Program Files\Git\bin\bash.exe`). A bare command like
        // `cmd /c ...` does NOT start at a root, so its launch exe stays
        // the first whitespace-delimited token.
        inline bool BeginsAtPathRoot(std::wstring_view commandline, size_t pos) noexcept
        {
            const size_t n = commandline.size();
            // UNC: \\server\share
            if (pos + 1 < n && commandline[pos] == L'\\' && commandline[pos + 1] == L'\\')
            {
                return true;
            }
            // Drive: X:\ or X:/
            if (pos + 2 < n)
            {
                const wchar_t d = commandline[pos];
                const bool isAlpha = (d >= L'A' && d <= L'Z') || (d >= L'a' && d <= L'z');
                if (isAlpha && commandline[pos + 1] == L':' &&
                    (commandline[pos + 2] == L'\\' || commandline[pos + 2] == L'/'))
                {
                    return true;
                }
            }
            return false;
        }

        // Returns the index just past the FIRST ".exe" (case-insensitive)
        // that is immediately followed by whitespace or end-of-string,
        // scanning from `start`. This locates the launch-executable
        // boundary in an unquoted path that contains spaces. Returns
        // std::wstring_view::npos when no such boundary exists.
        //
        // The launch exe wins over any ".exe" appearing later in the
        // arguments because we stop at the FIRST match: in
        // `C:\…\wsl.exe -e bash.exe` the scan stops at `wsl.exe`.
        inline size_t FindUnquotedLaunchExeEnd(std::wstring_view commandline, size_t start) noexcept
        {
            constexpr std::wstring_view dotExe{ L".exe" };
            const size_t n = commandline.size();
            if (n < dotExe.size())
            {
                return std::wstring_view::npos;
            }
            for (size_t i = start; i + dotExe.size() <= n; ++i)
            {
                bool extMatch = true;
                for (size_t j = 0; j < dotExe.size(); ++j)
                {
                    if (FoldAsciiLower(commandline[i + j]) != dotExe[j])
                    {
                        extMatch = false;
                        break;
                    }
                }
                if (!extMatch)
                {
                    continue;
                }
                const size_t after = i + dotExe.size();
                if (after == n || commandline[after] == L' ' || commandline[after] == L'\t')
                {
                    return after;
                }
            }
            return std::wstring_view::npos;
        }

        // Fallback for an unquoted rooted path whose launch executable is
        // written WITHOUT the `.exe` extension, e.g.
        //   C:\Program Files\Git\bin\bash -i -l
        // CreateProcessW resolves this by probing space-split prefixes and
        // auto-appending `.exe` (…\Program.exe fails, …\bin\bash.exe
        // resolves), so the profile launches `bash` — we must recognize it.
        // Returns the index just past the FIRST path segment `[\\/]<leaf>`
        // (case-insensitive) that is immediately followed by whitespace or
        // end-of-string, scanning from `start`; std::wstring_view::npos when
        // none exists.
        //
        // Requiring a leading separator AND a trailing whitespace/end keeps
        // this from matching a directory component (e.g. the "\PowerShell\"
        // folder when looking for `powershell`) or a bare argument token.
        // Only reached after FindUnquotedLaunchExeEnd has already failed, so
        // no earlier `.exe`-terminated launch exe can be shadowed. (A
        // contrived commandline with two rooted extensionless paths — a
        // launcher plus a path-shaped argument ending in `\<leaf>` — could
        // still false-match, but such a form never appears in a real profile
        // commandline.)
        inline size_t FindUnquotedLeafEnd(std::wstring_view commandline, size_t start, std::wstring_view leaf) noexcept
        {
            const size_t n = commandline.size();
            // Need at least a separator + the leaf.
            for (size_t i = start; i + 1 + leaf.size() <= n; ++i)
            {
                if (commandline[i] != L'\\' && commandline[i] != L'/')
                {
                    continue;
                }
                bool leafMatch = true;
                for (size_t j = 0; j < leaf.size(); ++j)
                {
                    if (FoldAsciiLower(commandline[i + 1 + j]) != FoldAsciiLower(leaf[j]))
                    {
                        leafMatch = false;
                        break;
                    }
                }
                if (!leafMatch)
                {
                    continue;
                }
                const size_t after = i + 1 + leaf.size();
                if (after == n || commandline[after] == L' ' || commandline[after] == L'\t')
                {
                    return after;
                }
            }
            return std::wstring_view::npos;
        }

        inline bool CommandlineHasExeToken(std::wstring_view commandline, std::wstring_view leaf) noexcept
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
            // Determine the end of the launch-executable token.
            size_t end = start;
            if (quoted)
            {
                // Quoted: token ends at the matching close-quote.
                while (end < commandline.size() && commandline[end] != L'"')
                {
                    ++end;
                }
            }
            else if (BeginsAtPathRoot(commandline, start))
            {
                // Unquoted path beginning at a filesystem root. The launch
                // executable may legitimately contain spaces, e.g.
                //   C:\Program Files\Git\bin\bash.exe -i -l
                // which CreateProcessW launches by probing progressively
                // longer space-split prefixes (…\Program.exe,
                // …\Program Files\Git.exe, … \bash.exe). Git installs under
                // "C:\Program Files\Git" by default, so this is the COMMON
                // form — NOT a malformed edge case. Splitting on the first
                // space would yield leaf "Program" and silently skip Git
                // Bash integration. Extend the token to the first ".exe"
                // boundary; fall back to first-whitespace when the path has
                // no extension (rare — CreateProcess would auto-append .exe).
                const size_t exeEnd = FindUnquotedLaunchExeEnd(commandline, start);
                if (exeEnd != std::wstring_view::npos)
                {
                    end = exeEnd;
                }
                else if (const size_t leafEnd = FindUnquotedLeafEnd(commandline, start, leaf);
                         leafEnd != std::wstring_view::npos)
                {
                    // Extensionless rooted launch exe (e.g.
                    // `C:\Program Files\Git\bin\bash -i -l`): match on the
                    // `\<leaf>` segment CreateProcess would resolve via
                    // .exe probing.
                    end = leafEnd;
                }
                else
                {
                    while (end < commandline.size() &&
                           commandline[end] != L' ' && commandline[end] != L'\t')
                    {
                        ++end;
                    }
                }
            }
            else
            {
                // Bare command (e.g. `pwsh -arg`, `bash.exe -i`, `cmd /c …`):
                // the launch exe is the first whitespace-delimited token.
                // It must NOT absorb a later `.exe` token in the arguments,
                // so we never run the path-root extension scan here.
                while (end < commandline.size() &&
                       commandline[end] != L' ' && commandline[end] != L'\t')
                {
                    ++end;
                }
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
            const auto equalsCi = [](std::wstring_view a, std::wstring_view b) noexcept -> bool {
                if (a.size() != b.size()) return false;
                for (size_t i = 0; i < a.size(); ++i)
                {
                    if (FoldAsciiLower(a[i]) != FoldAsciiLower(b[i])) return false;
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
                    if (FoldAsciiLower(leafToken[i]) != FoldAsciiLower(leaf[i])) { exeMatch = false; break; }
                }
                if (exeMatch)
                {
                    for (size_t i = 0; i < dotExe.size(); ++i)
                    {
                        if (FoldAsciiLower(leafToken[leaf.size() + i]) != dotExe[i]) { exeMatch = false; break; }
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
    // Matching strategy (intentionally simple — launch-exe leaf match
    // + one source discriminator — to avoid over-engineering this gate
    // while still recognizing both the path-with-.exe and bare forms
    // a user may legitimately set in their profile commandline):
    //
    //   * Pwsh: source == "Windows.Terminal.PowershellCore" OR
    //           launch-exe leaf is `pwsh` or `pwsh.exe`.
    //   * WindowsPowerShell: launch-exe leaf is `powershell` or
    //           `powershell.exe`. Note: pwsh.exe lives under
    //           "...\\PowerShell\\7\\pwsh.exe" but our matcher only
    //           inspects the LAUNCH leaf (first token after path-strip),
    //           and `pwsh.exe` ≠ `powershell.exe` as leaf tokens, so
    //           no NOT-pwsh discriminator is needed — pwsh launches
    //           naturally fail the powershell-leaf check.
    //   * Bash (Git Bash): launch-exe leaf is `bash` or `bash.exe`.
    //           WSL distro profiles whose launch is wsl.exe naturally
    //           fail this check (their leaf is `wsl(.exe)`, not
    //           `bash(.exe)`) — they're covered by the Wsl-source
    //           iteration on the caller side, not here.
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
    inline bool ProfileMatchesShell(Target target,
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
            return details::CommandlineHasExeToken(commandline, L"pwsh");
        case Target::WindowsPowerShell:
            // The launch-exe matcher compares the full leaf token
            // (`powershell` or `powershell.exe`) — a launch exe whose
            // leaf is `pwsh(.exe)` can't ALSO match `powershell(.exe)`
            // (different leaf token), so a `!HasExeToken(... "pwsh")`
            // discriminator is redundant. The substring-era matcher
            // needed it because pwsh.exe lives under a folder named
            // "PowerShell"; the launch-exe matcher anchors past that.
            return details::CommandlineHasExeToken(commandline, L"powershell");
        case Target::Bash:
            // Same reasoning as WindowsPowerShell: a launch exe with
            // leaf `bash(.exe)` cannot also have leaf `wsl(.exe)`, so
            // a `!HasExeToken(... "wsl")` check is redundant. WSL
            // distro profiles whose launch is wsl.exe naturally fail
            // the `bash` leaf check.
            return details::CommandlineHasExeToken(commandline, L"bash");
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
                    if (ProfileMatchesShell(target,
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

    // Resolves the install verdict for a single PowerShell host, keeping the
    // two concerns that must NOT be conflated cleanly separate:
    //
    //   * The $PROFILE WRITE is profile-gated — `performWrite` only runs when
    //     the user actually has a Windows Terminal profile launching this host
    //     (so we never append a pwsh block for someone who only uses Windows
    //     PowerShell, and vice-versa).
    //
    //   * The execution-policy VERDICT is UNCONDITIONAL. A Restricted /
    //     AllSigned policy means the shell-integration .ps1 can never run, so
    //     the FRE / Settings save MUST stop and surface the policy error even
    //     when no profile triggers a write. Tying this verdict to profile
    //     presence is exactly the regression this helper exists to prevent:
    //     the EP block was silently skipped (and reported as success) whenever
    //     `RunInstall` gated the host out, so the FRE never stopped.
    //
    // `executionPolicyBlocked` is supplied by the caller (a freshly-evaluated
    // `ExecutionPolicyBlocksShellIntegration(target)` probe) rather than
    // cached, so a user who fixes their policy offline and clicks Save again on
    // the SAME FRE is re-evaluated and allowed through.
    //
    // Order matters: the EP check is evaluated FIRST so a blocking policy
    // short-circuits before any write is attempted (a write would only
    // silent-no-op or throw PSSecurityException on every shell start anyway).
    //
    // `performWrite` is a callable returning InstallResult (e.g. a lambda
    // wrapping InstallForTarget); templated so unit tests can inject a counting
    // / sentinel double without spawning a real PowerShell.
    template<typename WriteFn>
    inline InstallResult ResolvePowerShellHostInstall(bool profilePresent,
                                                      bool executionPolicyBlocked,
                                                      WriteFn&& performWrite)
    {
        if (executionPolicyBlocked)
        {
            return { false, false, L"PowerShell execution policy blocks scripts", true };
        }
        if (profilePresent)
        {
            return std::forward<WriteFn>(performWrite)();
        }
        // Policy is fine and the user has no Windows Terminal profile for this
        // host — nothing to write. Report success-already-satisfied so the
        // sweep's all-installed verdict doesn't flag a missing shell.
        return { true, true, {}, false };
    }
}
