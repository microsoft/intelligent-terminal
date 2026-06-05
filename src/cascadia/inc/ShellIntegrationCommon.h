// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ShellIntegrationCommon.h
//
// Shared types, sentinel markers, the global profile-write mutex, and the
// flavor-agnostic Install / Uninstall driver used by all shell-integration
// flavors (PowerShell, Bash, WSL — see the per-flavor headers).
//
// Adding a new shell:
//   1. Build a FlavorDescriptor (script filename + content, block builder,
//      orphan-body recognizer, optional legacy-form detector, EOL policy,
//      friendly name for error messages).
//   2. Call InstallGeneric(profilePath, flavor) / UninstallGeneric(...).
//      Every cross-cutting concern (backup, EOL detection, mutex, orphan
//      recovery, idempotency, in-place vs append) is handled by the driver.

#pragma once

#include <array>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <functional>
#include <map>
#include <mutex>
#include <regex>
#include <string>
#include <string_view>
#include <utility>
#include <ShlObj.h>
#include <fmt/compile.h>
#include <fmt/format.h>
#include <fmt/xchar.h>
#include <wil/resource.h>

namespace Microsoft::Terminal::ShellIntegration
{
    enum class Target
    {
        Pwsh,
        WindowsPowerShell,
        Bash,
    };

    // Result of an installation attempt.
    struct InstallResult
    {
        bool success{ false };
        bool alreadyInstalled{ false }; // true when skipped because already configured
        std::wstring errorMessage;
        bool executionPolicyBlocked{ false }; // true when the host's effective PowerShell execution policy refuses unsigned local scripts
    };

    // ───────────────────────────────────────────────────────────────────
    // Sentinel markers bracketing the block we own in $PROFILE / ~/.bashrc.
    // Identical for every shell — both PowerShell and bash use `#` for
    // comments, so the same byte sequence is a valid no-op header in
    // both. The block BODY differs per shell (per-flavor BuildBlock).
    // ───────────────────────────────────────────────────────────────────
    inline constexpr std::string_view kShellIntegrationBlockOpenMarker =
        "# >>> intelligent-terminal shell-integration >>>";
    inline constexpr std::string_view kShellIntegrationBlockCloseMarker =
        "# <<< intelligent-terminal shell-integration <<<";

    // ───────────────────────────────────────────────────────────────────
    // FlavorDescriptor — per-shell knobs consumed by the generic driver.
    //
    // Why a struct of std::function rather than a virtual base class:
    //   • All shell-integration code is header-only / inline so it can
    //     be shared between FreOverlay and TerminalPage without a new
    //     translation unit. std::function preserves that.
    //   • Each flavor lives in its own header (BashShellIntegration.h
    //     etc.); none of them depend on each other.
    //   • Tests can craft a synthetic descriptor to exercise driver
    //     edge cases without spinning up a real shell.
    // ───────────────────────────────────────────────────────────────────
    struct FlavorDescriptor
    {
        // Where the versioned script file is written. For PowerShell this
        // is the parent of $PROFILE; for bash it is a dedicated dir
        // (~/.intelligent-terminal/); for WSL it is a UNC path into
        // \\wsl$\<distro>\.
        std::filesystem::path scriptDir;

        // Filename of the versioned script within scriptDir. Carries the
        // version (e.g. "shell-integration_v1.ps1" / ".sh") so old
        // versions left on disk after an upgrade stay inert (no one
        // references them).
        std::wstring scriptFileName;

        // The body of the script file. Pure ASCII / UTF-8.
        std::string scriptContent;

        // Build the sentinel-bracketed block to inject. Receives the
        // EOL the driver detected from the existing profile contents
        // ("\n" or "\r\n"). PS uses this to match the user's style;
        // bash ignores it and always emits LF inside the block (bash
        // files are never CRLF) — its EOL handling is via forceLf below.
        std::function<std::string(std::string_view eol)> buildBlock;

        // Predicate the orphan-marker recovery uses to decide whether a
        // candidate line is part of a (possibly-corrupted) block we
        // wrote. Each flavor knows the exact lines its BuildBlock
        // produces; the predicate matches by prefix / exact match / etc.
        // as appropriate for that flavor's body.
        std::function<bool(std::string_view candidateLine)> isOrphanBodyLine;

        // Optional legacy-form detector — returns the byte range of a
        // pre-modern reference to our scripts (e.g. the old PS
        // `. "...shell-integration*.ps1"` dot-source line) so the
        // installer can rewrite it in place. Leave unset for flavors
        // with no legacy form (bash v1 is the first release, etc).
        // When unset OR when it returns { npos, npos } and no modern
        // block is present, callers see "no existing block found".
        std::function<std::pair<size_t, size_t>(std::string_view contents)> findLegacy;

        // When true, force LF line endings regardless of what the
        // existing profile uses. Bash sets this; PS leaves it false
        // and lets the driver auto-detect from existing content.
        bool forceLf{ false };

        // Human-readable name of the profile file for error messages.
        // E.g. L"PowerShell profile" or L".bashrc". Spliced into
        // "Failed to read <profileFriendlyName>" etc.
        std::wstring profileFriendlyName;
    };

    namespace details
    {
        // Single global mutex serializing every profile-file mutation
        // across the process. Settings reload + FRE Save can both race
        // for the same $PROFILE; without this lock two writers could
        // each compute a new file body from the same baseline and the
        // second would clobber the first.
        inline std::mutex& ProfileFileMutex() noexcept
        {
            static std::mutex m;
            return m;
        }

        // Best-effort backup. Names the file
        // `.bak.<YYYYMMDD-HHMMSS>.<hashHex>` next to the profile.
        // Collision policy: the timestamp is second-resolution and the
        // hash narrows it further, but two backups of identical
        // content written within the same second WILL collide on
        // filename — the std::filesystem::copy_options::overwrite_existing
        // flag means the second call wins. That's intentional: when
        // contents are byte-identical, dropping the duplicate is fine.
        // Two backups of DIFFERENT content within the same second still
        // produce distinct filenames (different hash). Never throws;
        // never fails the caller — backup is a safety net, not a
        // correctness gate.
        inline void WriteBackup(const std::filesystem::path& profilePath, const std::string& contents) noexcept
        {
            try
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
                std::error_code ec;
                std::filesystem::copy_file(profilePath, backupPath,
                                           std::filesystem::copy_options::overwrite_existing, ec);
            }
            catch (...)
            {
                // Swallow — backup is best-effort.
            }
        }
    }

    // Locate the block we own in the given profile contents. Strategy:
    //   1. Modern: bytes between the two sentinel markers (inclusive).
    //   2. Orphan-recovery: open marker present, no close marker — take
    //      the marker line PLUS any following lines the flavor recognizes
    //      as body. Stops at the first unrecognized line so user content
    //      beneath the corruption is preserved.
    //   3. Legacy: if the flavor provides a `findLegacy` predicate, run
    //      it as a fallback (PS only, for migrating the original
    //      single-line dot-source form).
    //
    // Returns { npos, npos } when none match.
    inline std::pair<size_t, size_t> FindBlockGeneric(std::string_view contents, const FlavorDescriptor& flavor)
    {
        if (const auto openPos = contents.find(kShellIntegrationBlockOpenMarker);
            openPos != std::string::npos)
        {
            if (const auto closePos = contents.find(kShellIntegrationBlockCloseMarker, openPos);
                closePos != std::string::npos)
            {
                return { openPos, closePos + kShellIntegrationBlockCloseMarker.size() };
            }
            // Orphan open marker — consume marker line + recognized body.
            size_t lineEnd = openPos + kShellIntegrationBlockOpenMarker.size();
            while (lineEnd < contents.size() &&
                   contents[lineEnd] != '\n' &&
                   contents[lineEnd] != '\r')
            {
                ++lineEnd;
            }
            while (true)
            {
                size_t nextLineStart = lineEnd;
                if (nextLineStart < contents.size() && contents[nextLineStart] == '\r')
                {
                    ++nextLineStart;
                }
                if (nextLineStart < contents.size() && contents[nextLineStart] == '\n')
                {
                    ++nextLineStart;
                }
                if (nextLineStart == lineEnd || nextLineStart >= contents.size())
                {
                    break;
                }
                size_t candidateEnd = nextLineStart;
                while (candidateEnd < contents.size() &&
                       contents[candidateEnd] != '\n' &&
                       contents[candidateEnd] != '\r')
                {
                    ++candidateEnd;
                }
                const std::string_view candidate{
                    contents.data() + nextLineStart,
                    candidateEnd - nextLineStart
                };
                if (!flavor.isOrphanBodyLine || !flavor.isOrphanBodyLine(candidate))
                {
                    break;
                }
                lineEnd = candidateEnd;
            }
            return { openPos, lineEnd };
        }
        if (flavor.findLegacy)
        {
            return flavor.findLegacy(contents);
        }
        return { std::string::npos, std::string::npos };
    }

    // Install: ensure the profile + script exist and the block is in
    // place + matches the desired content. Idempotent — returns
    // alreadyInstalled=true when nothing needed changing.
    //
    // Flow:
    //   1. Ensure profile dir + script dir exist; touch an empty
    //      profile if it doesn't exist.
    //   2. Read profile contents.
    //   3. Detect EOL (CRLF if any in file, else LF — unless flavor.forceLf).
    //   4. Compute desired block via flavor.buildBlock(eol).
    //   5. Find existing block (modern markers OR flavor.findLegacy).
    //   6. If block matches desired AND script is on disk → no-op.
    //   7. Backup profile (non-fatal), write versioned script.
    //   8. Replace existing region OR append a new block.
    //   9. Write profile back; surface write/close errors.
    //
    // Synchronous — call from a background thread.
    inline InstallResult InstallGeneric(const std::wstring& profilePathW, const FlavorDescriptor& flavor)
    {
        if (profilePathW.empty())
        {
            return { false, false, L"Profile path is empty" };
        }
        if (flavor.scriptDir.empty())
        {
            return { false, false, L"Script directory is empty" };
        }

        std::lock_guard<std::mutex> profileGuard{ details::ProfileFileMutex() };

        const std::filesystem::path profilePath{ profilePathW };
        const auto profileDir = profilePath.parent_path();
        const auto scriptPath = flavor.scriptDir / flavor.scriptFileName;

        const auto formatFsError = [](std::wstring_view what,
                                      const std::filesystem::path& path,
                                      const std::error_code& ec) -> std::wstring {
            // ec.message() is std::string in the active ANSI codepage on
            // Windows (system_category messages may be localized — e.g.
            // German/Japanese/etc.). Per-byte widening would produce
            // mojibake for non-ASCII codepages. Use MultiByteToWideChar
            // with CP_ACP to widen correctly. If the conversion fails
            // (extremely rare), fall back to omitting the message — the
            // path + numeric error code is still actionable.
            //
            // We pass an explicit positive `cchSrc` (narrow.size())
            // rather than -1 so the returned `needed` count does NOT
            // include a trailing NUL. That avoids a 1-wchar overflow
            // class of bug (resize(needed-1) + buffer of length
            // `needed` with -1 input → writes past end).
            const auto narrow = ec.message();
            std::wstring widened;
            if (!narrow.empty())
            {
                const int srcLen = static_cast<int>(narrow.size());
                const int needed = MultiByteToWideChar(CP_ACP, 0,
                                                       narrow.c_str(), srcLen,
                                                       nullptr, 0);
                if (needed > 0)
                {
                    widened.resize(needed);
                    MultiByteToWideChar(CP_ACP, 0,
                                        narrow.c_str(), srcLen,
                                        widened.data(), needed);
                }
            }
            std::wstring out{ what };
            out += L" '" + path.wstring() + L"': ";
            out += std::to_wstring(ec.value());
            if (!widened.empty())
            {
                out += L' ';
                out += widened;
            }
            return out;
        };

        std::error_code ec;
        if (!profileDir.empty())
        {
            std::filesystem::create_directories(profileDir, ec);
            if (ec)
            {
                return { false, false,
                         formatFsError(L"Failed to create profile directory", profileDir, ec) };
            }
        }
        std::filesystem::create_directories(flavor.scriptDir, ec);
        if (ec)
        {
            return { false, false,
                     formatFsError(L"Failed to create script directory", flavor.scriptDir, ec) };
        }

        {
            std::error_code existsEc;
            // Use the non-throwing overload — std::filesystem::exists()
            // without an error_code can throw filesystem_error on
            // access failures (notably UNC providers like \\wsl$\... or
            // a network filesystem timing out). The installer is best-
            // effort and must NOT crash the app; treat any failure to
            // determine existence as "doesn't exist" and try to create.
            if (!std::filesystem::exists(profilePath, existsEc))
            {
                std::ofstream{ profilePath, std::ios::binary }; // touch
            }
        }

        std::string contents;
        {
            std::ifstream in{ profilePath, std::ios::binary };
            if (!in)
            {
                return { false, false, L"Failed to open " + flavor.profileFriendlyName + L" for reading" };
            }
            contents.assign(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
            if (in.bad())
            {
                return { false, false, L"Failed to read " + flavor.profileFriendlyName };
            }
        }

        const std::string_view eol = (!flavor.forceLf && contents.find("\r\n") != std::string::npos)
            ? std::string_view{ "\r\n" }
            : std::string_view{ "\n" };

        const auto desiredBlock = flavor.buildBlock(eol);
        const auto [lineStart, lineEnd] = FindBlockGeneric(contents, flavor);
        const bool found = lineStart != std::string::npos;

        if (found &&
            std::string_view(contents.data() + lineStart, lineEnd - lineStart) == desiredBlock)
        {
            // Non-throwing exists() check for the script file — same
            // reason as the profilePath check above: must not crash
            // the app on transient UNC / network I/O failures. Treat a
            // failed existence check as "missing" and proceed to
            // rewrite the script.
            std::error_code scriptExistsEc;
            if (std::filesystem::exists(scriptPath, scriptExistsEc))
            {
                return { true, true, {} };
            }
        }

        if (!contents.empty())
        {
            details::WriteBackup(profilePath, contents);
        }

        {
            std::ofstream scriptOut{ scriptPath, std::ios::binary | std::ios::trunc };
            if (!scriptOut)
            {
                return { false, false, L"Failed to write shell-integration script" };
            }
            scriptOut.write(flavor.scriptContent.data(), flavor.scriptContent.size());
            scriptOut.close();
            if (!scriptOut)
            {
                return { false, false, L"Failed to write shell-integration script (write/close failed)" };
            }
        }

        if (found)
        {
            contents.replace(lineStart, lineEnd - lineStart, desiredBlock);
        }
        else
        {
            if (!contents.empty() && contents.back() != '\n')
            {
                contents += eol;
            }
            contents += desiredBlock;
            contents += eol;
        }

        std::ofstream profileOut{ profilePath, std::ios::binary | std::ios::trunc };
        if (!profileOut)
        {
            return { false, false, L"Failed to write " + flavor.profileFriendlyName };
        }
        profileOut.write(contents.data(), contents.size());
        profileOut.close();
        if (!profileOut)
        {
            return { false, false, L"Failed to write " + flavor.profileFriendlyName + L" (write/close failed)" };
        }
        return { true, false, {} };
    }

    // Uninstall: strip the block from the profile. The versioned script
    // file is intentionally LEFT on disk — inert once the source line is
    // gone, and leaving it avoids needless writes that could touch
    // OneDrive-synced content.
    //
    // `alreadyInstalled` is reused to mean "already in the desired state"
    // (i.e. no block present, nothing to remove). Idempotent.
    //
    // Synchronous — call from a background thread.
    inline InstallResult UninstallGeneric(const std::wstring& profilePathW, const FlavorDescriptor& flavor)
    {
        if (profilePathW.empty())
        {
            return { false, false, L"Profile path is empty" };
        }

        std::lock_guard<std::mutex> profileGuard{ details::ProfileFileMutex() };

        const std::filesystem::path profilePath{ profilePathW };

        std::error_code ec;
        const bool profileExists = std::filesystem::exists(profilePath, ec);
        if (ec)
        {
            return { false, false, L"Failed to stat " + flavor.profileFriendlyName };
        }
        if (!profileExists)
        {
            return { true, true, {} };
        }

        std::string contents;
        {
            std::ifstream in{ profilePath, std::ios::binary };
            if (!in)
            {
                return { false, false, L"Failed to open " + flavor.profileFriendlyName + L" for reading" };
            }
            contents.assign(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
            if (in.bad())
            {
                return { false, false, L"Failed to read " + flavor.profileFriendlyName };
            }
        }

        const auto [lineStart, lineEnd] = FindBlockGeneric(contents, flavor);
        if (lineStart == std::string::npos)
        {
            return { true, true, {} };
        }

        // Consume the line terminator that immediately follows the block
        // so we don't leave an orphan empty line behind.
        size_t removeEnd = lineEnd;
        if (removeEnd < contents.size() && contents[removeEnd] == '\r')
        {
            ++removeEnd;
        }
        if (removeEnd < contents.size() && contents[removeEnd] == '\n')
        {
            ++removeEnd;
        }

        details::WriteBackup(profilePath, contents);

        contents.erase(lineStart, removeEnd - lineStart);

        std::ofstream profileOut{ profilePath, std::ios::binary | std::ios::trunc };
        if (!profileOut)
        {
            return { false, false, L"Failed to write " + flavor.profileFriendlyName };
        }
        profileOut.write(contents.data(), contents.size());
        profileOut.close();
        if (!profileOut)
        {
            return { false, false, L"Failed to write " + flavor.profileFriendlyName + L" (write/close failed)" };
        }
        return { true, false, {} };
    }
}
