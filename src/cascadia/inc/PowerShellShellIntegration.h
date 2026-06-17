// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PowerShellShellIntegration.h
//
// PowerShell flavor of the shell integration installer. Exposes two
// concrete IShellFlavor classes — PowerShellFlavor (pwsh / PS 7+) and
// WindowsPowerShellFlavor (PS 5.1) — that the orchestrator drives.
//
// The two hosts use different $PROFILE subdirs (Documents/PowerShell vs
// Documents/WindowsPowerShell) but share script content, block format,
// marker recognizers, and the v0→v1 legacy dot-source migration path.

#pragma once

#include "ShellIntegrationCommon.h"

namespace Microsoft::Terminal::ShellIntegration::Powershell
{
    namespace details
    {
        // Runs `<exe> -NoProfile -NonInteractive -Command Get-ExecutionPolicy`
        // synchronously and returns the lowercased policy name from stdout
        // (e.g. "restricted"). Returns an empty string if the executable can't
        // be launched (typical for pwsh.exe when PowerShell 7 isn't installed)
        // or if the call doesn't finish within the timeout.
        //
        // `-Command <expr>` runs an inline expression that is NOT subject to
        // the .ps1 execution policy, so this works even when the answer is
        // Restricted / AllSigned. We deliberately do NOT pass
        // `-ExecutionPolicy` because that would set the Process scope and
        // override the value we're trying to read.
        inline std::wstring QueryExecutionPolicy(LPCWSTR exe) noexcept
        {
            // This is a best-effort helper: any failure (CreateProcess, pipe,
            // read hang, OOM, …) must fail-open by returning an empty string
            // so the caller treats the policy as "not blocking" rather than
            // crashing the Terminal over a diagnostic probe.
            try
            {
                SECURITY_ATTRIBUTES sa{};
                sa.nLength = sizeof(sa);
                sa.bInheritHandle = TRUE;

                HANDLE rawRead = nullptr;
                HANDLE rawWrite = nullptr;
                if (!CreatePipe(&rawRead, &rawWrite, &sa, 0))
                {
                    return {};
                }
                wil::unique_handle readEnd{ rawRead };
                wil::unique_handle writeEnd{ rawWrite };
                SetHandleInformation(readEnd.get(), HANDLE_FLAG_INHERIT, 0);

                STARTUPINFOW si{};
                si.cb = sizeof(si);
                si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
                si.wShowWindow = SW_HIDE;
                si.hStdOutput = writeEnd.get();
                si.hStdError = writeEnd.get();
                si.hStdInput = GetStdHandle(STD_INPUT_HANDLE);

                std::wstring cmdLine{ L"\"" };
                cmdLine += exe;
                cmdLine += L"\" -NoProfile -NonInteractive -Command Get-ExecutionPolicy";

                PROCESS_INFORMATION pi{};
                if (!CreateProcessW(nullptr,
                                    cmdLine.data(),
                                    nullptr,
                                    nullptr,
                                    TRUE,
                                    CREATE_NO_WINDOW,
                                    nullptr,
                                    nullptr,
                                    &si,
                                    &pi))
                {
                    return {};
                }
                wil::unique_handle process{ pi.hProcess };
                wil::unique_handle thread{ pi.hThread };

                writeEnd.reset();

                if (WaitForSingleObject(process.get(), 5000) != WAIT_OBJECT_0)
                {
                    TerminateProcess(process.get(), 1);
                    WaitForSingleObject(process.get(), 1000);
                }

                std::string raw;
                char buf[256];
                DWORD bytesRead = 0;
                while (raw.size() < 4096 &&
                       ReadFile(readEnd.get(), buf, sizeof(buf), &bytesRead, nullptr) &&
                       bytesRead > 0)
                {
                    raw.append(buf, bytesRead);
                }

                std::wstring result;
                for (const char c : raw)
                {
                    if (c == '\r' || c == '\n')
                    {
                        if (!result.empty())
                        {
                            break;
                        }
                        continue;
                    }
                    if (c >= 'A' && c <= 'Z')
                    {
                        result.push_back(static_cast<wchar_t>(c + 0x20));
                    }
                    else if (c >= 'a' && c <= 'z')
                    {
                        result.push_back(static_cast<wchar_t>(c));
                    }
                }
                return result;
            }
            catch (...)
            {
                return {};
            }
        }

        inline bool PolicyNameBlocksUnsignedScripts(std::wstring_view name) noexcept
        {
            // Allow-list: only these three policies permit unsigned local
            // scripts (like our shell-integration profile) to execute.
            // Everything else — including the empty string returned when
            // QueryExecutionPolicy fails in packaged-app context — is
            // treated as blocking so FRE surfaces the actionable EP error
            // instead of silently writing a profile that will never load.
            return !(name == L"remotesigned" || name == L"unrestricted" || name == L"bypass");
        }

        // Body-line recognizer for orphan-marker recovery — matches the
        // exact line prefixes the block builder emits. The `$__it_si`
        // / `Remove-Variable __it_si` prefixes are private to us so
        // collisions with user content are not a realistic concern.
        inline bool IsOrphanBodyLine(std::string_view candidate) noexcept
        {
            constexpr std::array<std::string_view, 6> bodyPrefixes = {
                std::string_view{ "# Auto-generated by Intelligent Terminal" },
                std::string_view{ "# Documents is resolved at runtime" },
                std::string_view{ "# Folder Move and is a silent no-op" },
                std::string_view{ "$__it_si " },
                std::string_view{ "if (Test-Path -LiteralPath $__it_si)" },
                std::string_view{ "Remove-Variable __it_si" },
            };
            for (const auto& prefix : bodyPrefixes)
            {
                if (candidate.size() >= prefix.size() &&
                    candidate.substr(0, prefix.size()) == prefix)
                {
                    return true;
                }
            }
            return false;
        }

        // Legacy detector: `. "...shell-integration*.ps1"` dot-source
        // line. Detection (and rewrite-on-install) is how existing
        // affected profiles get migrated to the modern block.
        //
        // Pattern: line begins with `.` + whitespace + a quoted path
        // whose FINAL filename component is `shell-integration*.ps1`.
        // The path-component check (preceded by `/`, `\`, or the
        // opening quote; followed only by non-separator chars before
        // `.ps1`) avoids false matches on directories that happen to
        // contain "shell-integration".
        //
        // `(^|\n)` substitutes for the C++17 `multiline` flag — MSVC's
        // STL does NOT define `std::regex_constants::multiline`. We
        // trim the consumed `\n` out of the returned range so callers
        // see only the dot-source line itself.
        inline std::optional<std::pair<size_t, size_t>> FindLegacyDotSource(std::string_view contents)
        {
            static const std::regex pattern{
                R"((^|\n)[ \t]*\.[ \t]+"(?:[^"]*[\\/])?shell-integration[^"\\/]*\.ps1".*)",
                std::regex_constants::ECMAScript
            };
            std::cmatch m;
            if (std::regex_search(contents.data(), contents.data() + contents.size(), m, pattern))
            {
                size_t start = static_cast<size_t>(m.position());
                size_t end = start + static_cast<size_t>(m.length());
                if (start < contents.size() && contents[start] == '\n')
                {
                    ++start;
                }
                while (end > start && contents[end - 1] == '\r')
                {
                    --end;
                }
                return std::make_pair(start, end);
            }
            return std::nullopt;
        }
    }

    // True when the effective PowerShell execution policy for `target` refuses
    // to run unsigned local scripts. Asks PowerShell itself rather than walking
    // the registry / Group Policy hives — `Get-ExecutionPolicy` returns the
    // effective policy after considering every scope plus the built-in default.
    //
    // Re-queried on every call so that after the user fixes the policy outside
    // (e.g. `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned`) and clicks
    // Save again, the Terminal picks up the new policy.
    inline bool ExecutionPolicyBlocksShellIntegration(Target target) noexcept
    {
        const auto exe = target == Target::Pwsh ? L"pwsh.exe" : L"powershell.exe";
        // If the host binary isn't on PATH, we can't (and shouldn't) probe
        // its policy. Return false so a missing optional host (e.g. pwsh.exe
        // on machines without PowerShell 7) doesn't false-positive as
        // "EP blocked". QueryExecutionPolicy would return "" for a missing
        // host, and the allow-list predicate treats "" as blocking — correct
        // for a PRESENT host whose policy probe failed in packaged-app
        // context, but wrong for a genuinely absent host.
        wchar_t buf[MAX_PATH]{};
        if (SearchPathW(nullptr, exe, nullptr, MAX_PATH, buf, nullptr) == 0)
        {
            return false;
        }
        return details::PolicyNameBlocksUnsignedScripts(details::QueryExecutionPolicy(exe));
    }

    // Discover the PowerShell $PROFILE path.
    // Uses SHGetKnownFolderPath for the Documents folder instead of spawning
    // a shell process, which hangs indefinitely in packaged-app environments.
    // SHGetKnownFolderPath respects OneDrive redirection and group policy.
    inline std::wstring DiscoverProfilePath(Target target)
    {
        wil::unique_cotaskmem_string documentsPath;
        if (FAILED(SHGetKnownFolderPath(FOLDERID_Documents, 0, nullptr, &documentsPath)) || !documentsPath)
        {
            return {};
        }
        std::filesystem::path profilePath{ documentsPath.get() };
        profilePath /= (target == Target::Pwsh) ? L"PowerShell" : L"WindowsPowerShell";
        profilePath /= L"Microsoft.PowerShell_profile.ps1";
        return profilePath.wstring();
    }

    // ───────────────────────────────────────────────────────────────────
    // SINGLE SOURCE OF TRUTH for the PowerShell shell-integration script
    // version. The version is carried by the filename
    // (`shell-integration_vN.ps1`) — installs detect any prior
    // `shell-integration*.ps1` reference in $PROFILE and rewrite it to
    // point at the current version. Older script files left on disk are
    // inert (never referenced). To roll out a new version, bump this.
    // ───────────────────────────────────────────────────────────────────
    inline constexpr int kVersion = 1;

    inline std::wstring ScriptFileName()
    {
        return L"shell-integration_v" + std::to_wstring(kVersion) + L".ps1";
    }

    // Build the $PROFILE block. The block resolves Documents at runtime
    // via [Environment]::GetFolderPath('MyDocuments') so it:
    //   • survives OneDrive Known Folder Move enabled AFTER install
    //   • is a silent no-op (via Test-Path guard) on roamed profiles
    //     reaching a machine without Intelligent Terminal installed
    //   • respects Group Policy folder redirection to a network share
    inline std::string BuildBlock(std::wstring_view profileSubdir, std::string_view eol)
    {
        const auto fileName = til::u16u8(ScriptFileName());
        const auto subdir = til::u16u8(std::wstring{ profileSubdir });

        std::string block;
        block += kShellIntegrationBlockOpenMarker;                                          block += eol;
        block += "# Auto-generated by Intelligent Terminal. Do not edit between markers.";  block += eol;
        block += "# Documents is resolved at runtime so this survives OneDrive Known";      block += eol;
        block += "# Folder Move and is a silent no-op on machines without IT installed.";   block += eol;
        block += "$__it_si = Join-Path ([Environment]::GetFolderPath('MyDocuments')) '";
        block += subdir;
        block += "\\";
        block += fileName;
        block += "'";                                                                       block += eol;
        block += "if (Test-Path -LiteralPath $__it_si) { . $__it_si }";                     block += eol;
        block += "Remove-Variable __it_si -ErrorAction SilentlyContinue";                   block += eol;
        block += kShellIntegrationBlockCloseMarker;
        return block;
    }

    // The shell integration script content. The version is carried by the
    // filename, not embedded inside the script body.
    inline std::string ScriptContent()
    {
        return std::string{
            R"(# Shell Integration — non-invasive prompt wrapper
# Emits OSC 133 (command marks / exit code) and OSC 9;9 (CWD) escape
# sequences WITHOUT altering the visual appearance of the user's prompt.
#
# Compatible with Windows PowerShell 5.1+ and PowerShell 7+.
# Safe to source multiple times (idempotent guard).

if (-not $Global:__ShellInteg_Installed) {

    # ── Escape characters (PS 5.1 doesn't support `e / `a literals) ──
    $Global:__ShellInteg_ESC = [char]0x1B   # ESC
    $Global:__ShellInteg_BEL = [char]0x07   # BEL (OSC string terminator)

    # ── Snapshot the user's current prompt before we touch it ──────────
    $Global:__ShellInteg_OriginalPrompt = $function:prompt
    $Global:__ShellInteg_LastHistoryId  = -1
    $Global:__ShellInteg_Installed      = $true

    function Global:__ShellInteg_GetLastExitCode {
        # $? still reflects the *user's* last command here because this
        # is the very first call inside the prompt function.
        if ($? -eq $True) { return 0 }
        $entry = Get-History -Count 1
        if ($entry -and $Error[0].InvocationInfo.HistoryId -eq $entry.Id) {
            return -1          # PowerShell-level error
        }
        return $LastExitCode   # native command exit code
    }

    function prompt {
        # ── Capture exit code FIRST — before anything else can clobber $? ──
        $gle   = $(__ShellInteg_GetLastExitCode)
        $entry = Get-History -Count 1
        $loc   = $executionContext.SessionState.Path.CurrentLocation
        $E     = $Global:__ShellInteg_ESC
        $B     = $Global:__ShellInteg_BEL

        $prefix = ''
        $suffix = ''

        # ── Previous command finished (OSC 133;D with exit code) ──
        if ($entry -and $entry.Id -ne $Global:__ShellInteg_LastHistoryId) {
            $prefix += "${E}]133;D;${gle}${B}"
        }

        # ── Prompt started (OSC 133;A) ──
        $prefix += "${E}]133;A${B}"

        # ── Report current working directory (OSC 9;9) ──
        $prefix += "${E}]9;9;`"${loc}`"${B}"

        # ── Prompt ended, command input starts (OSC 133;B) ──
        $suffix = "${E}]133;B${B}"

        # ── Delegate to the user's ORIGINAL prompt — visual output is theirs ──
        $originalOutput = & $Global:__ShellInteg_OriginalPrompt

        $Global:__ShellInteg_LastHistoryId = if ($entry) { $entry.Id } else { -1 }

        return "${prefix}${originalOutput}${suffix}"
    }
}
)"
        };
    }

    // Shared base for the two concrete PowerShell flavors. Holds the
    // profile path and the subdir name embedded in the generated block;
    // every IShellFlavor method other than ProfilePath / the subdir is
    // identical between pwsh and Windows PowerShell.
    //
    // Not part of the public API surface — call sites construct the
    // concrete PowerShellFlavor or WindowsPowerShellFlavor.
    class PowerShellFlavorBase : public IShellFlavor
    {
    public:
        std::wstring          ProfilePath() const override          { return _profilePath; }
        std::filesystem::path ScriptDir() const override            { return std::filesystem::path{ _profilePath }.parent_path(); }
        std::wstring          ScriptFileName() const override       { return Powershell::ScriptFileName(); }
        std::string           ScriptContent() const override        { return Powershell::ScriptContent(); }
        std::wstring          ProfileFriendlyName() const override  { return L"PowerShell profile"; }
        LineEndingPolicy      LineEndings() const override          { return LineEndingPolicy::Auto; }

        std::string ScriptBlock(std::string_view eol) const override
        {
            return Powershell::BuildBlock(_profileSubdir, eol);
        }

        std::optional<std::pair<size_t, size_t>>
        FindExistingScriptBlock(std::string_view contents) const override
        {
            return ::Microsoft::Terminal::ShellIntegration::details::FindBlock(
                contents,
                &details::IsOrphanBodyLine,
                &details::FindLegacyDotSource);
        }

    protected:
        PowerShellFlavorBase(std::wstring profilePath, std::wstring profileSubdir) :
            _profilePath{ std::move(profilePath) },
            _profileSubdir{ std::move(profileSubdir) }
        {
        }

    private:
        std::wstring _profilePath;
        std::wstring _profileSubdir; // "PowerShell" or "WindowsPowerShell"
    };

    // PowerShell 7+ ($PROFILE under Documents\PowerShell\).
    //
    // The subdir name baked into the generated block is derived from
    // the profile path's parent dir (so a test that points at
    // `<tmp>\PowerShell\…` produces a block referencing PowerShell
    // and a test pointing at `<tmp>\WindowsPowerShell\…` produces one
    // referencing WindowsPowerShell). This matches the pre-refactor
    // behavior the FindBlock + BuildBlock tests rely on.
    class PowerShellFlavor : public PowerShellFlavorBase
    {
    public:
        explicit PowerShellFlavor(std::wstring profilePath) :
            PowerShellFlavorBase{ std::move(profilePath), _SubdirFromPath(profilePath) }
        {
        }

    private:
        static std::wstring _SubdirFromPath(const std::wstring& profilePath)
        {
            const auto subdir = std::filesystem::path{ profilePath }.parent_path().filename().wstring();
            return subdir.empty() ? std::wstring{ L"PowerShell" } : subdir;
        }
    };

    // Windows PowerShell 5.1 ($PROFILE under Documents\WindowsPowerShell\).
    class WindowsPowerShellFlavor : public PowerShellFlavorBase
    {
    public:
        explicit WindowsPowerShellFlavor(std::wstring profilePath) :
            PowerShellFlavorBase{ std::move(profilePath), _SubdirFromPath(profilePath) }
        {
        }

    private:
        static std::wstring _SubdirFromPath(const std::wstring& profilePath)
        {
            const auto subdir = std::filesystem::path{ profilePath }.parent_path().filename().wstring();
            return subdir.empty() ? std::wstring{ L"WindowsPowerShell" } : subdir;
        }
    };

    // Path-taking convenience used by both the FRE / Settings code
    // paths and the umbrella Install / Uninstall flat aliases that
    // the tests call. Picks the right concrete flavor based on the
    // profile's parent dir name.
    inline InstallResult Install(const std::wstring& profilePathW)
    {
        if (profilePathW.empty())
        {
            return { false, false, L"Profile path is empty" };
        }
        const auto subdir = std::filesystem::path{ profilePathW }.parent_path().filename().wstring();
        if (subdir == L"WindowsPowerShell")
        {
            WindowsPowerShellFlavor flavor{ profilePathW };
            return orchestrator::Install(flavor);
        }
        PowerShellFlavor flavor{ profilePathW };
        return orchestrator::Install(flavor);
    }

    inline InstallResult Uninstall(const std::wstring& profilePathW)
    {
        if (profilePathW.empty())
        {
            return { false, false, L"Profile path is empty" };
        }
        const auto subdir = std::filesystem::path{ profilePathW }.parent_path().filename().wstring();
        if (subdir == L"WindowsPowerShell")
        {
            WindowsPowerShellFlavor flavor{ profilePathW };
            return orchestrator::Uninstall(flavor);
        }
        PowerShellFlavor flavor{ profilePathW };
        return orchestrator::Uninstall(flavor);
    }

    // Convenience: discover + install. Probes execution policy first so
    // a Restricted host fails up front with a specific error rather than
    // succeeding-then-silently-erroring on every shell start.
    inline InstallResult InstallForTarget(Target target)
    {
        if (ExecutionPolicyBlocksShellIntegration(target))
        {
            return { false, false, L"PowerShell execution policy blocks scripts", true };
        }
        auto profilePath = DiscoverProfilePath(target);
        if (profilePath.empty())
        {
            return { false, false, L"Could not discover PowerShell profile path" };
        }
        return Install(profilePath);
    }

    inline InstallResult UninstallForTarget(Target target)
    {
        auto profilePath = DiscoverProfilePath(target);
        if (profilePath.empty())
        {
            return { false, false, L"Could not discover PowerShell profile path" };
        }
        return Uninstall(profilePath);
    }
}
