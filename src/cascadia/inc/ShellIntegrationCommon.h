// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ShellIntegrationCommon.h
//
// Shared types, sentinel markers, the global profile-write mutex, and the
// flavor-agnostic Install / Uninstall orchestrator used by every shell-
// integration flavor (PowerShell, Bash, WSL — see the per-flavor headers).
//
// Adding a new shell:
//   1. Subclass IShellFlavor and implement the description-only methods
//      (where the script lives, what the script body is, how to build /
//      recognize the block we own in the user's profile).
//   2. Call orchestrator::Install / orchestrator::Uninstall with an
//      instance of your flavor. Every cross-cutting concern (backup,
//      EOL detection, mutex, orphan recovery, idempotency, in-place vs
//      append, missing-script repair) is handled by the orchestrator.

#pragma once

#include <array>
#include <chrono>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <functional>
#include <map>
#include <mutex>
#include <optional>
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

    // How the orchestrator should pick the EOL it writes inside the
    // managed block. Auto = match what the existing profile uses (CRLF
    // if any CRLF is present, else LF). Lf = always LF, regardless —
    // used by bash where files are never CRLF.
    enum class LineEndingPolicy
    {
        Auto,
        Lf,
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
    // both. The block BODY differs per shell (per-flavor ScriptBlock).
    // ───────────────────────────────────────────────────────────────────
    inline constexpr std::string_view kShellIntegrationBlockOpenMarker =
        "# >>> intelligent-terminal shell-integration >>>";
    inline constexpr std::string_view kShellIntegrationBlockCloseMarker =
        "# <<< intelligent-terminal shell-integration <<<";

    // ───────────────────────────────────────────────────────────────────
    // IShellFlavor — pure description of one shell-integration target.
    //
    // No Install / Uninstall verbs: the orchestrator owns all of those.
    // The flavor only answers "where do my files live", "what do I
    // write", and "what range of bytes inside this profile content
    // belongs to me".
    //
    // Header-only / inline so flavors can be shared between FreOverlay
    // and TerminalPage without a new translation unit. The interface is
    // small enough that the per-call virtual dispatch is irrelevant
    // (installs run on a background thread, once per shell).
    // ───────────────────────────────────────────────────────────────────
    class IShellFlavor
    {
    public:
        virtual ~IShellFlavor() = default;

        // Path to the user's profile file we'll inject into
        // ($PROFILE / ~/.bashrc / \\wsl$\<distro>\…\.bashrc).
        virtual std::wstring ProfilePath() const = 0;

        // Where the versioned script file is written. For PowerShell
        // this is the parent of $PROFILE; for bash it is a dedicated
        // dir (~/.intelligent-terminal/); for WSL it is a UNC path
        // into \\wsl$\<distro>\.
        virtual std::filesystem::path ScriptDir() const = 0;

        // Filename of the versioned script within ScriptDir(). Carries
        // the version (e.g. "shell-integration_v1.ps1" / ".sh") so old
        // versions left on disk after an upgrade stay inert (no one
        // references them).
        virtual std::wstring ScriptFileName() const = 0;

        // The body of the script file. Pure ASCII / UTF-8.
        virtual std::string ScriptContent() const = 0;

        // Human-readable name of the profile file for error messages.
        // E.g. L"PowerShell profile" or L".bashrc". Spliced into
        // "Failed to read <ProfileFriendlyName>" etc.
        virtual std::wstring ProfileFriendlyName() const = 0;

        // How the orchestrator should pick the EOL it writes (see
        // LineEndingPolicy). Bash returns Lf; PowerShell returns Auto.
        virtual LineEndingPolicy LineEndings() const = 0;

        // Build the sentinel-bracketed block to inject. Receives the
        // EOL the orchestrator selected per LineEndings() and the
        // profile's existing content ("\n" or "\r\n").
        virtual std::string ScriptBlock(std::string_view eol) const = 0;

        // Locate the byte range of any existing block we own inside
        // `contents`. Strategy is flavor-defined: every flavor must
        // recognize the modern sentinel-bracketed form and orphan-
        // marker recovery (open marker + recognized body lines, no
        // close marker). PowerShell additionally recognizes the
        // legacy `. "...shell-integration*.ps1"` dot-source line.
        //
        // Returns std::nullopt when no existing region is found —
        // installer treats this as "append a fresh block".
        virtual std::optional<std::pair<size_t, size_t>>
        FindExistingScriptBlock(std::string_view contents) const = 0;
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

        // Shared marker-block + orphan-recovery scanner. Each flavor's
        // FindExistingScriptBlock funnels through this with its own
        // body-line predicate and optional legacy detector — the
        // recovery shape is identical across flavors, only the
        // recognized body lines differ.
        //
        // Strategy:
        //   1. Modern: bytes between the two sentinel markers (inclusive).
        //   2. Orphan-recovery: open marker present, no close marker —
        //      take the marker line PLUS any following lines the
        //      flavor recognizes as body. Stops at the first
        //      unrecognized line so user content beneath the
        //      corruption is preserved.
        //   3. Legacy: if `legacyFinder` is provided, run it as a
        //      fallback (PS only, for migrating the original
        //      single-line dot-source form).
        //
        // Returns std::nullopt when nothing matches.
        inline std::optional<std::pair<size_t, size_t>> FindBlock(
            std::string_view contents,
            const std::function<bool(std::string_view)>& isOrphanBodyLine,
            const std::function<std::optional<std::pair<size_t, size_t>>(std::string_view)>& legacyFinder)
        {
            if (const auto openPos = contents.find(kShellIntegrationBlockOpenMarker);
                openPos != std::string::npos)
            {
                if (const auto closePos = contents.find(kShellIntegrationBlockCloseMarker, openPos);
                    closePos != std::string::npos)
                {
                    return std::make_pair(openPos, closePos + kShellIntegrationBlockCloseMarker.size());
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
                    if (!isOrphanBodyLine || !isOrphanBodyLine(candidate))
                    {
                        break;
                    }
                    lineEnd = candidateEnd;
                }
                return std::make_pair(openPos, lineEnd);
            }
            if (legacyFinder)
            {
                return legacyFinder(contents);
            }
            return std::nullopt;
        }
    }

    namespace orchestrator
    {
        // Install: ensure the profile + script exist and the block is
        // in place + matches the desired content. Idempotent — returns
        // alreadyInstalled=true when nothing needed changing.
        //
        // Flow:
        //   1. Ensure profile dir + script dir exist; touch an empty
        //      profile if it doesn't exist.
        //   2. Read profile contents.
        //   3. Detect EOL per flavor.LineEndings() (CRLF if Auto and
        //      any CRLF in file, else LF).
        //   4. Compute desired block via flavor.ScriptBlock(eol).
        //   5. Find existing block via flavor.FindExistingScriptBlock.
        //   6. If block matches desired AND script is on disk → no-op;
        //      else if profile matches but script is missing → repair
        //      script only (don't rewrite the profile).
        //   7. Backup profile (non-fatal), write versioned script.
        //   8. Replace existing region OR append a new block.
        //   9. Write profile back; surface write/close errors.
        //
        // Synchronous — call from a background thread.
        inline InstallResult Install(const IShellFlavor& flavor)
        {
            const auto profilePathW = flavor.ProfilePath();
            const auto scriptDir = flavor.ScriptDir();
            const auto friendlyName = flavor.ProfileFriendlyName();

            if (profilePathW.empty())
            {
                return { false, false, L"Profile path is empty" };
            }
            if (scriptDir.empty())
            {
                return { false, false, L"Script directory is empty" };
            }

            std::lock_guard<std::mutex> profileGuard{ details::ProfileFileMutex() };

            const std::filesystem::path profilePath{ profilePathW };
            const auto profileDir = profilePath.parent_path();
            const auto scriptPath = scriptDir / flavor.ScriptFileName();

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
            std::filesystem::create_directories(scriptDir, ec);
            if (ec)
            {
                return { false, false,
                         formatFsError(L"Failed to create script directory", scriptDir, ec) };
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
                    return { false, false, L"Failed to open " + friendlyName + L" for reading" };
                }
                contents.assign(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
                if (in.bad())
                {
                    return { false, false, L"Failed to read " + friendlyName };
                }
            }

            const std::string_view eol = (flavor.LineEndings() == LineEndingPolicy::Auto &&
                                          contents.find("\r\n") != std::string::npos)
                ? std::string_view{ "\r\n" }
                : std::string_view{ "\n" };

            const auto desiredBlock = flavor.ScriptBlock(eol);
            const auto existing = flavor.FindExistingScriptBlock(contents);
            const bool found = existing.has_value();

            if (found &&
                std::string_view(contents.data() + existing->first, existing->second - existing->first) == desiredBlock)
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
                // Script-only repair: profile contents already match
                // desiredBlock byte-for-byte. Don't back up or rewrite the
                // profile — that would be a no-op write that still produces
                // a `.bak.*` file and an mtime bump on the user's PROFILE
                // (potentially OneDrive-synced). Just write the missing
                // script file and return.
                std::ofstream scriptRepairOut{ scriptPath, std::ios::binary | std::ios::trunc };
                if (!scriptRepairOut)
                {
                    return { false, false, L"Failed to write shell-integration script" };
                }
                const auto scriptContent = flavor.ScriptContent();
                scriptRepairOut.write(scriptContent.data(), scriptContent.size());
                scriptRepairOut.close();
                if (!scriptRepairOut)
                {
                    return { false, false, L"Failed to write shell-integration script (write/close failed)" };
                }
                return { true, false, {} };
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
                const auto scriptContent = flavor.ScriptContent();
                scriptOut.write(scriptContent.data(), scriptContent.size());
                scriptOut.close();
                if (!scriptOut)
                {
                    return { false, false, L"Failed to write shell-integration script (write/close failed)" };
                }
            }

            if (found)
            {
                contents.replace(existing->first, existing->second - existing->first, desiredBlock);
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
                return { false, false, L"Failed to write " + friendlyName };
            }
            profileOut.write(contents.data(), contents.size());
            profileOut.close();
            if (!profileOut)
            {
                return { false, false, L"Failed to write " + friendlyName + L" (write/close failed)" };
            }
            return { true, false, {} };
        }

        // Uninstall: strip the block from the profile. The versioned
        // script file is intentionally LEFT on disk — inert once the
        // source line is gone, and leaving it avoids needless writes
        // that could touch OneDrive-synced content.
        //
        // `alreadyInstalled` is reused to mean "already in the desired
        // state" (i.e. no block present, nothing to remove). Idempotent.
        //
        // Synchronous — call from a background thread.
        inline InstallResult Uninstall(const IShellFlavor& flavor)
        {
            const auto profilePathW = flavor.ProfilePath();
            const auto friendlyName = flavor.ProfileFriendlyName();

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
                return { false, false, L"Failed to stat " + friendlyName };
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
                    return { false, false, L"Failed to open " + friendlyName + L" for reading" };
                }
                contents.assign(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
                if (in.bad())
                {
                    return { false, false, L"Failed to read " + friendlyName };
                }
            }

            const auto existing = flavor.FindExistingScriptBlock(contents);
            if (!existing)
            {
                return { true, true, {} };
            }

            // Consume the line terminator that immediately follows the
            // block so we don't leave an orphan empty line behind.
            size_t removeEnd = existing->second;
            if (removeEnd < contents.size() && contents[removeEnd] == '\r')
            {
                ++removeEnd;
            }
            if (removeEnd < contents.size() && contents[removeEnd] == '\n')
            {
                ++removeEnd;
            }

            details::WriteBackup(profilePath, contents);

            contents.erase(existing->first, removeEnd - existing->first);

            std::ofstream profileOut{ profilePath, std::ios::binary | std::ios::trunc };
            if (!profileOut)
            {
                return { false, false, L"Failed to write " + friendlyName };
            }
            profileOut.write(contents.data(), contents.size());
            profileOut.close();
            if (!profileOut)
            {
                return { false, false, L"Failed to write " + friendlyName + L" (write/close failed)" };
            }
            return { true, false, {} };
        }
    }
}
