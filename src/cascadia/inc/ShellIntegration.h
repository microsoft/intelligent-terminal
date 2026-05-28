// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ShellIntegration.h
//
// Pure Win32 + STL functions for installing PowerShell shell integration
// scripts (OSC 133 prompt marks). Shared by FreOverlay (FRE wizard) and
// TerminalPage (Settings UI).
//
// The shell integration script wraps the user's prompt to emit:
//   OSC 133;D;<exit_code>  — command finished (triggers autofix)
//   OSC 133;A              — prompt started
//   OSC 133;B              — command input starts
//   OSC 9;9;"<cwd>"        — current working directory

#pragma once

#include <filesystem>
#include <fstream>
#include <regex>
#include <string>
#include <string_view>
#include <utility>
#include <ShlObj.h>

namespace Microsoft::Terminal::ShellIntegration
{
    enum class Target
    {
        Pwsh,
        WindowsPowerShell,
    };

    // Result of an installation attempt.
    struct InstallResult
    {
        bool success{ false };
        bool alreadyInstalled{ false }; // true when skipped because already configured
        std::wstring errorMessage;
    };

    // Discover the PowerShell $PROFILE path.
    // Uses SHGetKnownFolderPath for the Documents folder instead of spawning
    // a shell process, which hangs indefinitely in packaged-app environments
    // (confirmed on both our FRE code and the remote's _InitShellIntegration).
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
    // SINGLE SOURCE OF TRUTH for shell-integration script versioning.
    // The version is carried by the filename (`shell-integration_vN.ps1`)
    // — Install() detects any prior `shell-integration*.ps1` dot-source
    // line in $PROFILE and rewrites it to point at the current version.
    // Older script files left on disk are inert (never referenced).
    // To roll out a new version, bump this integer.
    // ───────────────────────────────────────────────────────────────────
    inline constexpr int kShellIntegrationVersion = 1;

    // Versioned filename — derived from kShellIntegrationVersion.
    inline std::wstring ShellIntegrationScriptFileName()
    {
        return L"shell-integration_v" + std::to_wstring(kShellIntegrationVersion) + L".ps1";
    }

    // The shell integration script content. The version is carried by the
    // filename, not embedded inside the script body.
    inline std::wstring ShellIntegrationScriptContent()
    {
        return std::wstring{
            LR"(# Shell Integration — non-invasive prompt wrapper
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

    // Locates an existing `. "...shell-integration*.ps1"` dot-source line in `contents`.
    // Returns the [start, end) byte range of the line (CR/LF excluded), or
    // { npos, npos } when no such line is present. Matches any version of our
    // script — current (shell-integration_vN.ps1) AND legacy (shell-integration.ps1)
    // — so upgrades can rewrite the line in place.
    //
    // Pattern: line begins with `.` + whitespace + a quoted path whose
    // FINAL filename component is `shell-integration*.ps1`. The path
    // component check (preceded by `/`, `\`, or the opening quote;
    // followed only by non-separator chars before `.ps1`) avoids false
    // matches on directories that happen to contain "shell-integration"
    // or on trailing comments after an unrelated dot-source line.
    //
    // `(^|\n)` substitutes for the C++17 `multiline` flag — MSVC's
    // STL does NOT define `std::regex_constants::multiline` (only the
    // basic POSIX flags + icase/nosubs/optimize/collate), so the
    // documented C++17 spelling does not compile here. We instead match
    // start-of-string OR a literal newline, and trim the consumed `\n`
    // out of the returned range so callers still see only the
    // dot-source line itself.
    inline std::pair<size_t, size_t> FindShellIntegrationDotSourceLine(std::string_view contents)
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
            // If the alternation matched `\n` (non-first-line case), the
            // newline is at `contents[start]`; advance past it so the
            // returned range starts on the dot-source line proper.
            if (start < contents.size() && contents[start] == '\n')
            {
                ++start;
            }
            // `.` doesn't match `\n`, so the match naturally stops at end-of-line.
            // For CRLF input a trailing `\r` may remain in the captured range — strip it.
            while (end > start && contents[end - 1] == '\r')
            {
                --end;
            }
            return { start, end };
        }
        return { std::string::npos, std::string::npos };
    }

    // Install shell integration for a given PowerShell profile path.
    // Writes the versioned script (named via ShellIntegrationScriptFileName())
    // next to the profile and ensures $PROFILE dot-sources it. Idempotent —
    // returns alreadyInstalled=true when the existing dot-source line already
    // references the current script and the script file is on disk.
    //
    // Flow (one pass, file-not-exists case collapses into the existing-file case):
    //   1. Ensure profile dir + file exist (touch an empty file if missing).
    //   2. Read profile contents.
    //   3. Find any existing `. "...shell-integration*.ps1"` line.
    //   4. If it already matches the desired line and the script is on disk → no-op.
    //   5. Backup profile (non-fatal), write the versioned script.
    //   6. Replace the existing line in place, OR append a new line at the bottom.
    //   7. Write the profile back.
    //
    // Synchronous — call from a background thread.
    inline InstallResult Install(const std::wstring& profilePathW)
    {
        if (profilePathW.empty())
        {
            return { false, false, L"Profile path is empty" };
        }

        const std::filesystem::path profilePath{ profilePathW };
        const auto profileDir = profilePath.parent_path();
        const auto scriptPath = profileDir / ShellIntegrationScriptFileName();

        // 1. Ensure profile dir + file exist. A freshly-touched file is just
        //    empty content — the rest of the flow handles it identically.
        std::error_code ec;
        std::filesystem::create_directories(profileDir, ec);
        if (ec)
        {
            return { false, false, L"Failed to create profile directory" };
        }
        if (!std::filesystem::exists(profilePath))
        {
            std::ofstream{ profilePath, std::ios::binary }; // touch
        }

        // 2. Read profile contents.
        std::string contents;
        {
            std::ifstream in{ profilePath, std::ios::binary };
            if (!in)
            {
                return { false, false, L"Failed to open PowerShell profile for reading" };
            }
            contents.assign(std::istreambuf_iterator<char>(in),
                            std::istreambuf_iterator<char>());
            if (in.bad())
            {
                return { false, false, L"Failed to read PowerShell profile" };
            }
        }

        // Detect existing line-ending style so the appended line matches.
        // If the profile contains any CRLF, treat it as a CRLF file.
        const std::string_view eol = contents.find("\r\n") != std::string::npos
            ? std::string_view{ "\r\n" }
            : std::string_view{ "\n" };

        // 3. Find any existing dot-source line (current or legacy version).
        const auto desiredLine = til::u16u8(
            fmt::format(FMT_COMPILE(L". \"{}\""), scriptPath.wstring()));
        const auto [lineStart, lineEnd] = FindShellIntegrationDotSourceLine(contents);
        const bool found = lineStart != std::string::npos;

        // 4. No-op when the existing line already matches AND the script is on disk.
        if (found &&
            std::string_view(contents.data() + lineStart, lineEnd - lineStart) == desiredLine &&
            std::filesystem::exists(scriptPath))
        {
            return { true, true, {} };
        }

        // 5a. Backup $PROFILE before modifying (non-fatal if it fails).
        if (!contents.empty())
        {
            const auto now = std::chrono::system_clock::now();
            const auto tt = std::chrono::system_clock::to_time_t(now);
            struct tm tm{};
            localtime_s(&tm, &tt);
            wchar_t timeBuf[32]{};
            wcsftime(timeBuf, std::size(timeBuf), L"%Y%m%d-%H%M%S", &tm);

            const auto contentHash = std::hash<std::string>{}(contents);
            auto backupPath = profilePath.wstring() +
                L".bak." + timeBuf + L"." +
                fmt::format(FMT_COMPILE(L"{:08x}"), contentHash & 0xFFFFFFFF);
            std::filesystem::copy_file(profilePath, backupPath,
                                       std::filesystem::copy_options::overwrite_existing, ec);
        }

        // 5b. Write (or refresh) the versioned script next to the profile.
        {
            std::ofstream scriptOut{ scriptPath, std::ios::binary | std::ios::trunc };
            if (!scriptOut)
            {
                return { false, false, L"Failed to write shell-integration script" };
            }
            const auto scriptUtf8 = til::u16u8(ShellIntegrationScriptContent());
            scriptOut.write(scriptUtf8.data(), scriptUtf8.size());
            scriptOut.close();
            if (!scriptOut)
            {
                return { false, false, L"Failed to write shell-integration script (write/close failed)" };
            }
        }

        // 6. Update existing line in place, or append a new line at the bottom.
        if (found)
        {
            contents.replace(lineStart, lineEnd - lineStart, desiredLine);
        }
        else
        {
            if (!contents.empty() && contents.back() != '\n')
            {
                contents += eol;
            }
            contents += desiredLine;
            contents += eol;
        }

        // 7. Write the profile back. Check write/close — opening with `trunc`
        //    means a silent mid-stream failure would leave the profile
        //    truncated or partial. Surface any failure so callers don't
        //    report a successful install over a corrupt profile.
        std::ofstream profileOut{ profilePath, std::ios::binary | std::ios::trunc };
        if (!profileOut)
        {
            return { false, false, L"Failed to write PowerShell profile" };
        }
        profileOut.write(contents.data(), contents.size());
        profileOut.close();
        if (!profileOut)
        {
            return { false, false, L"Failed to write PowerShell profile (write/close failed)" };
        }
        return { true, false, {} };
    }

    // Convenience: discover profile path + install, for a given target.
    // Synchronous — call from a background thread.
    inline InstallResult InstallForTarget(Target target)
    {
        auto profilePath = DiscoverProfilePath(target);
        if (profilePath.empty())
        {
            return { false, false, L"Could not discover PowerShell profile path" };
        }
        return Install(profilePath);
    }
}
