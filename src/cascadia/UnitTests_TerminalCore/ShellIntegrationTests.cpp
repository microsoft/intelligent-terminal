// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Unit tests for src/cascadia/inc/ShellIntegration.h.
//
// These tests exercise the path-taking Install / Uninstall overloads only.
// They NEVER call DiscoverProfilePath / InstallForTarget / UninstallForTarget,
// so the developer's real $PROFILE is never touched. Each test operates
// inside a unique per-test temp directory under std::filesystem::temp_directory_path().

#include "pch.h"
#include <WexTestClass.h>

#include <atomic>
#include <filesystem>
#include <fstream>
#include <string>
#include <string_view>

#include "../inc/ShellIntegration.h"

using namespace WEX::Common;
using namespace WEX::Logging;
using namespace WEX::TestExecution;

using namespace Microsoft::Terminal::ShellIntegration;

namespace TerminalCoreUnitTests
{
    class ShellIntegrationTests;
};
using namespace TerminalCoreUnitTests;

class TerminalCoreUnitTests::ShellIntegrationTests final
{
    TEST_CLASS(ShellIntegrationTests);

    // FindShellIntegrationBlock — pure parser.
    TEST_METHOD(FindBlock_EmptyContent_ReturnsNpos);
    TEST_METHOD(FindBlock_UnrelatedContent_ReturnsNpos);
    TEST_METHOD(FindBlock_ModernBlock_ReturnsRange);
    TEST_METHOD(FindBlock_OrphanOpenMarker_ConsumesRecognizableBodyLines);
    TEST_METHOD(FindBlock_OrphanOpenMarker_StopsAtUnrelatedUserContent);
    TEST_METHOD(FindBlock_LegacyDotSource_ReturnsLineRange);
    TEST_METHOD(FindBlock_LegacyDotSource_FirstLine_ReturnsLineRange);
    TEST_METHOD(FindBlock_LegacyDotSource_CrlfPreservesRange);
    TEST_METHOD(FindBlock_FalsePositive_DirectoryNameContainingShellIntegration);

    // BuildShellIntegrationBlock — pure generator.
    TEST_METHOD(BuildBlock_ContainsMarkersAndScriptFilename);
    TEST_METHOD(BuildBlock_HonoursEolParameter);

    // Install scenarios.
    TEST_METHOD(Install_EmptyPath_Fails);
    TEST_METHOD(Install_ProfileMissing_CreatesProfileAndScript);
    TEST_METHOD(Install_ProfileWithoutBlock_AppendsBlockPreservesOriginalContent);
    TEST_METHOD(Install_PreservesCrlfFromExistingProfile);
    TEST_METHOD(Install_PreservesLfFromExistingProfile);
    TEST_METHOD(Install_AppendsEolWhenProfileMissingTrailingNewline);
    TEST_METHOD(Install_IdempotentWhenAlreadyInstalled);
    TEST_METHOD(Install_ReinstallsWhenScriptMissingButBlockMatches);
    TEST_METHOD(Install_RewritesLegacyDotSourceLineInPlace);
    TEST_METHOD(Install_OverwritesOrphanOpenMarker);
    TEST_METHOD(Install_CreatesBackupForNonEmptyProfile);
    TEST_METHOD(Install_DoesNotCreateBackupForEmptyProfile);
    TEST_METHOD(Install_TwoConsecutiveCalls_AreIdempotent);

    // Uninstall scenarios.
    TEST_METHOD(Uninstall_EmptyPath_Fails);
    TEST_METHOD(Uninstall_ProfileMissing_NoOp);
    TEST_METHOD(Uninstall_ProfileWithoutBlock_NoOp);
    TEST_METHOD(Uninstall_StripsModernBlockCleanly);
    TEST_METHOD(Uninstall_StripsBlockInMiddleOfFile);
    TEST_METHOD(Uninstall_StripsLegacyDotSourceLine);
    TEST_METHOD(Uninstall_StripsOrphanOpenMarkerAndRecognizableBody);
    TEST_METHOD(Uninstall_LeavesUnrelatedTailAfterOrphanCleanup);
    TEST_METHOD(Uninstall_CreatesBackupBeforeMutating);
    TEST_METHOD(Uninstall_AfterInstall_RestoresOriginalContent);
    TEST_METHOD(Uninstall_TwoConsecutiveCalls_AreIdempotent);

    // Install -> Uninstall -> Install round-trip
    TEST_METHOD(InstallUninstallInstall_RoundTrip);

    // ExecutionPolicy detection.
    TEST_METHOD(PolicyName_RestrictedAndAllSigned_AreBlocking);
    TEST_METHOD(PolicyName_RemoteSignedAndPermissive_AreNotBlocking);
    TEST_METHOD(PolicyName_EmptyOrUnknown_NotBlocking);
    TEST_METHOD(QueryExecutionPolicy_NonexistentExe_ReturnsEmpty);
    TEST_METHOD(QueryExecutionPolicy_ParsesStdoutAndLowercases);
    TEST_METHOD(QueryExecutionPolicy_TrimsWhitespaceAndStopsAtFirstLine);

    // ─── Bash flavor ──────────────────────────────────────────────────────
    // FindShellIntegrationBashBlock — pure parser.
    TEST_METHOD(Bash_FindBlock_EmptyContent_ReturnsNpos);
    TEST_METHOD(Bash_FindBlock_UnrelatedContent_ReturnsNpos);
    TEST_METHOD(Bash_FindBlock_ModernBlock_ReturnsRange);
    TEST_METHOD(Bash_FindBlock_OrphanOpenMarker_ConsumesRecognizableBodyLines);
    TEST_METHOD(Bash_FindBlock_OrphanOpenMarker_StopsAtUnrelatedUserContent);

    // BuildShellIntegrationBashBlock + ShellIntegrationBashScriptContent — generators.
    TEST_METHOD(Bash_BuildBlock_ContainsMarkersAndScriptFilename);
    TEST_METHOD(Bash_BuildBlock_IsLfOnly);
    TEST_METHOD(Bash_BuildBlock_UsesHomeAndGuardsOnBashVersion);
    TEST_METHOD(Bash_ScriptContent_HasIdempotencyGuardAndOscSequences);

    // InstallBash / UninstallBash scenarios.
    TEST_METHOD(Bash_Install_EmptyProfilePath_Fails);
    TEST_METHOD(Bash_Install_EmptyScriptDir_Fails);
    TEST_METHOD(Bash_Install_ProfileMissing_CreatesProfileAndScript);
    TEST_METHOD(Bash_Install_ProfileWithoutBlock_AppendsBlockPreservesOriginalContent);
    TEST_METHOD(Bash_Install_IsLfOnly);
    TEST_METHOD(Bash_Install_IdempotentWhenAlreadyInstalled);
    TEST_METHOD(Bash_Install_ReinstallsWhenScriptMissingButBlockMatches);
    TEST_METHOD(Bash_Install_OverwritesOrphanOpenMarker);
    TEST_METHOD(Bash_Install_CreatesBackupForNonEmptyProfile);
    TEST_METHOD(Bash_Install_DoesNotCreateBackupForEmptyProfile);

    TEST_METHOD(Bash_Uninstall_EmptyPath_Fails);
    TEST_METHOD(Bash_Uninstall_ProfileMissing_NoOp);
    TEST_METHOD(Bash_Uninstall_ProfileWithoutBlock_NoOp);
    TEST_METHOD(Bash_Uninstall_StripsBlockCleanly);
    TEST_METHOD(Bash_Uninstall_AfterInstall_RestoresOriginalContent);
    TEST_METHOD(Bash_Uninstall_TwoConsecutiveCalls_AreIdempotent);

    TEST_METHOD(Bash_InstallUninstallInstall_RoundTrip);

    // ─── WSL flavor (helpers only — Install/UninstallWslBash requires real WSL) ──
    TEST_METHOD(Wsl_IsSafeDistroName_AcceptsCommonNames);
    TEST_METHOD(Wsl_IsSafeDistroName_RejectsInjection);
    TEST_METHOD(Wsl_IsSafeDistroName_RejectsEmptyAndOverlong);
    TEST_METHOD(Wsl_IsSafeWslHome_AcceptsCommonHomes);
    TEST_METHOD(Wsl_IsSafeWslHome_RejectsRelativeAndTraversal);
    TEST_METHOD(Wsl_IsSafeWslHome_RejectsBadChars);
    TEST_METHOD(Wsl_UncPath_BuildsExpectedFormat);
    TEST_METHOD(Wsl_InstallWslBash_RejectsUnsafeDistroName);
    TEST_METHOD(Wsl_UninstallWslBash_RejectsUnsafeDistroName);

    TEST_CLASS_SETUP(ClassSetup)
    {
        return true;
    }

    TEST_METHOD_SETUP(MethodSetup)
    {
        _scratchDir = _MakeUniqueScratchDir();
        std::error_code ec;
        std::filesystem::create_directories(_scratchDir, ec);
        return !ec;
    }

    TEST_METHOD_CLEANUP(MethodCleanup)
    {
        std::error_code ec;
        std::filesystem::remove_all(_scratchDir, ec);
        // Cleanup failures are non-fatal — tests should still pass even if a
        // file is briefly locked by AV.
        return true;
    }

private:
    std::filesystem::path _scratchDir;

    static std::filesystem::path _MakeUniqueScratchDir()
    {
        // Each test gets a unique subdir so parallel runs / leftover state
        // never bleed across tests. We deliberately avoid CoCreateGuid /
        // StringFromGUID2 here so this test project doesn't take an
        // ole32.lib dependency it doesn't otherwise need.
        static std::atomic<uint64_t> counter{ 0 };
        wchar_t buf[64]{};
        swprintf_s(buf,
                   L"%lu-%llu-%llu",
                   ::GetCurrentProcessId(),
                   static_cast<unsigned long long>(::GetTickCount64()),
                   static_cast<unsigned long long>(counter.fetch_add(1, std::memory_order_relaxed)));
        return std::filesystem::temp_directory_path() / L"ShellIntegrationTests" / buf;
    }

    // Build a profile path inside a "PowerShell" sub-folder so the
    // BuildShellIntegrationBlock-emitted subdir matches what real callers
    // would see (the subdir name is derived from the parent folder).
    std::filesystem::path _ProfilePath(std::wstring_view subdir = L"PowerShell") const
    {
        return _scratchDir / subdir / L"Microsoft.PowerShell_profile.ps1";
    }

    // Bash equivalents: .bashrc lives at the scratch root, and the
    // versioned .sh lives under a sibling "bash-script-dir" so tests
    // never touch the real %USERPROFILE%\.intelligent-terminal\.
    std::filesystem::path _BashProfilePath() const
    {
        return _scratchDir / L".bashrc";
    }
    std::filesystem::path _BashScriptDir() const
    {
        return _scratchDir / L"bash-script-dir";
    }

    static std::string _ReadFile(const std::filesystem::path& p)
    {
        std::ifstream in{ p, std::ios::binary };
        return { std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>() };
    }

    static void _WriteFile(const std::filesystem::path& p, std::string_view contents)
    {
        std::error_code ec;
        std::filesystem::create_directories(p.parent_path(), ec);
        std::ofstream out{ p, std::ios::binary | std::ios::trunc };
        out.write(contents.data(), contents.size());
    }

    static bool _Contains(std::string_view haystack, std::string_view needle)
    {
        return haystack.find(needle) != std::string_view::npos;
    }

    // Count files in `dir` whose name starts with `<profileName>.bak.`.
    static size_t _CountBackups(const std::filesystem::path& profilePath)
    {
        size_t n = 0;
        const auto prefix = profilePath.filename().wstring() + L".bak.";
        std::error_code ec;
        for (const auto& entry : std::filesystem::directory_iterator{ profilePath.parent_path(), ec })
        {
            if (entry.path().filename().wstring().rfind(prefix, 0) == 0)
            {
                ++n;
            }
        }
        return n;
    }
};

// ─── FindShellIntegrationBlock ────────────────────────────────────────────────

void ShellIntegrationTests::FindBlock_EmptyContent_ReturnsNpos()
{
    const auto [s, e] = FindShellIntegrationBlock("");
    VERIFY_ARE_EQUAL(std::string::npos, s);
    VERIFY_ARE_EQUAL(std::string::npos, e);
}

void ShellIntegrationTests::FindBlock_UnrelatedContent_ReturnsNpos()
{
    const auto [s, e] = FindShellIntegrationBlock("Write-Host 'hello'\nSet-Location ~\n");
    VERIFY_ARE_EQUAL(std::string::npos, s);
    VERIFY_ARE_EQUAL(std::string::npos, e);
}

void ShellIntegrationTests::FindBlock_ModernBlock_ReturnsRange()
{
    std::string content = "Write-Host 'pre'\n";
    const auto blockStart = content.size();
    content += std::string{ kShellIntegrationBlockOpenMarker };
    content += "\nbody\n";
    content += std::string{ kShellIntegrationBlockCloseMarker };
    const auto blockEnd = content.size();
    content += "\nWrite-Host 'post'\n";

    const auto [s, e] = FindShellIntegrationBlock(content);
    VERIFY_ARE_EQUAL(blockStart, s);
    VERIFY_ARE_EQUAL(blockEnd, e);
}

void ShellIntegrationTests::FindBlock_OrphanOpenMarker_ConsumesRecognizableBodyLines()
{
    // Simulate an interrupted Install: open marker + body lines we
    // would have emitted, but no close marker. FindShellIntegrationBlock
    // must return the full corrupted region so callers can replace OR
    // strip it without leaving executable dot-source lines behind.
    std::string content = "before\n";
    const auto blockStart = content.size();
    content += std::string{ kShellIntegrationBlockOpenMarker };
    content += "\n# Auto-generated by Intelligent Terminal. Do not edit between markers.";
    content += "\n# Documents is resolved at runtime so this survives OneDrive Known";
    content += "\n# Folder Move and is a silent no-op on machines without IT installed.";
    content += "\n$__it_si = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'PowerShell\\foo.ps1'";
    content += "\nif (Test-Path -LiteralPath $__it_si) { . $__it_si }";
    content += "\nRemove-Variable __it_si -ErrorAction SilentlyContinue";
    const auto blockEnd = content.size();
    content += "\nWrite-Host 'post'\n";

    const auto [s, e] = FindShellIntegrationBlock(content);
    VERIFY_ARE_EQUAL(blockStart, s);
    VERIFY_ARE_EQUAL(blockEnd, e, L"Orphan range must engulf all recognizable body lines");
}

void ShellIntegrationTests::FindBlock_OrphanOpenMarker_StopsAtUnrelatedUserContent()
{
    // Orphan body followed immediately by user content (no blank line):
    // scanning must stop at the first non-body line so user code is
    // preserved when Install/Uninstall operate on the returned range.
    std::string content;
    const auto blockStart = content.size();
    content += std::string{ kShellIntegrationBlockOpenMarker };
    content += "\n$__it_si = 'leaked'";
    const auto blockEnd = content.size();
    content += "\nSet-Alias ll Get-ChildItem\n";

    const auto [s, e] = FindShellIntegrationBlock(content);
    VERIFY_ARE_EQUAL(blockStart, s);
    VERIFY_ARE_EQUAL(blockEnd, e, L"Scan must stop at first non-body line");
}

void ShellIntegrationTests::FindBlock_LegacyDotSource_ReturnsLineRange()
{
    const std::string content =
        "Write-Host 'pre'\n"
        ". \"C:\\Users\\me\\Documents\\PowerShell\\shell-integration_v1.ps1\"\n"
        "Write-Host 'post'\n";

    const auto [s, e] = FindShellIntegrationBlock(content);
    VERIFY_ARE_NOT_EQUAL(std::string::npos, s);
    const auto matched = content.substr(s, e - s);
    VERIFY_IS_TRUE(_Contains(matched, "shell-integration"));
    VERIFY_IS_FALSE(_Contains(matched, "Write-Host"), L"Match must not engulf neighbour lines");
    VERIFY_ARE_EQUAL('.', matched.front());
}

void ShellIntegrationTests::FindBlock_LegacyDotSource_FirstLine_ReturnsLineRange()
{
    const std::string content =
        ". \"C:\\Users\\me\\Documents\\PowerShell\\shell-integration.ps1\"\n"
        "Write-Host 'post'\n";

    const auto [s, e] = FindShellIntegrationBlock(content);
    VERIFY_ARE_EQUAL(static_cast<size_t>(0), s);
    const auto matched = content.substr(s, e - s);
    VERIFY_ARE_EQUAL('.', matched.front());
    VERIFY_IS_FALSE(_Contains(matched, "Write-Host"));
}

void ShellIntegrationTests::FindBlock_LegacyDotSource_CrlfPreservesRange()
{
    const std::string content =
        "Write-Host 'pre'\r\n"
        ". \"C:\\Users\\me\\Documents\\PowerShell\\shell-integration_v1.ps1\"\r\n"
        "Write-Host 'post'\r\n";

    const auto [s, e] = FindShellIntegrationBlock(content);
    VERIFY_ARE_NOT_EQUAL(std::string::npos, s);
    const auto matched = content.substr(s, e - s);
    VERIFY_ARE_EQUAL('.', matched.front());
    VERIFY_ARE_NOT_EQUAL('\r', matched.back(), L"Trailing \\r must be trimmed from match");
}

void ShellIntegrationTests::FindBlock_FalsePositive_DirectoryNameContainingShellIntegration()
{
    // A *directory* called "shell-integration-stuff" should NOT count as a
    // managed dot-source line; the regex requires the FILENAME component
    // to start with `shell-integration`.
    const std::string content =
        ". \"C:\\Users\\me\\shell-integration-stuff\\my-script.ps1\"\n";

    const auto [s, e] = FindShellIntegrationBlock(content);
    VERIFY_ARE_EQUAL(std::string::npos, s);
    VERIFY_ARE_EQUAL(std::string::npos, e);
}

// ─── BuildShellIntegrationBlock ───────────────────────────────────────────────

void ShellIntegrationTests::BuildBlock_ContainsMarkersAndScriptFilename()
{
    const auto block = BuildShellIntegrationBlock(L"PowerShell", "\n");
    VERIFY_IS_TRUE(_Contains(block, kShellIntegrationBlockOpenMarker));
    VERIFY_IS_TRUE(_Contains(block, kShellIntegrationBlockCloseMarker));
    VERIFY_IS_TRUE(_Contains(block, "PowerShell\\"));
    // Block embeds the versioned script filename.
    const auto fileName = til::u16u8(ShellIntegrationScriptFileName());
    VERIFY_IS_TRUE(_Contains(block, fileName));
}

void ShellIntegrationTests::BuildBlock_HonoursEolParameter()
{
    const auto lf = BuildShellIntegrationBlock(L"PowerShell", "\n");
    const auto crlf = BuildShellIntegrationBlock(L"PowerShell", "\r\n");
    VERIFY_IS_FALSE(_Contains(lf, "\r\n"), L"LF block must not contain CRLF");
    VERIFY_IS_TRUE(_Contains(crlf, "\r\n"), L"CRLF block must contain CRLF separators");
}

// ─── Install ──────────────────────────────────────────────────────────────────

void ShellIntegrationTests::Install_EmptyPath_Fails()
{
    const auto r = Install(L"");
    VERIFY_IS_FALSE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);
    VERIFY_IS_FALSE(r.errorMessage.empty());
}

void ShellIntegrationTests::Install_ProfileMissing_CreatesProfileAndScript()
{
    const auto profile = _ProfilePath();
    VERIFY_IS_FALSE(std::filesystem::exists(profile));

    const auto r = Install(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);
    VERIFY_IS_TRUE(std::filesystem::exists(profile));
    VERIFY_IS_TRUE(std::filesystem::exists(profile.parent_path() / ShellIntegrationScriptFileName()));

    const auto contents = _ReadFile(profile);
    VERIFY_IS_TRUE(_Contains(contents, kShellIntegrationBlockOpenMarker));
    VERIFY_IS_TRUE(_Contains(contents, kShellIntegrationBlockCloseMarker));
}

void ShellIntegrationTests::Install_ProfileWithoutBlock_AppendsBlockPreservesOriginalContent()
{
    const auto profile = _ProfilePath();
    const std::string original = "Set-Alias ll Get-ChildItem\nWrite-Host 'hi'\n";
    _WriteFile(profile, original);

    const auto r = Install(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);

    const auto contents = _ReadFile(profile);
    VERIFY_IS_TRUE(contents.rfind(original, 0) == 0, L"Original content must remain at start of file");
    VERIFY_IS_TRUE(_Contains(contents, kShellIntegrationBlockOpenMarker));
    VERIFY_IS_TRUE(_Contains(contents, kShellIntegrationBlockCloseMarker));
}

void ShellIntegrationTests::Install_PreservesCrlfFromExistingProfile()
{
    const auto profile = _ProfilePath();
    _WriteFile(profile, "Write-Host 'hi'\r\n");

    VERIFY_IS_TRUE(Install(profile.wstring()).success);

    const auto contents = _ReadFile(profile);
    // No bare LF inside our block (each LF must be preceded by CR).
    const auto openPos = contents.find(kShellIntegrationBlockOpenMarker);
    const auto closePos = contents.find(kShellIntegrationBlockCloseMarker, openPos);
    VERIFY_ARE_NOT_EQUAL(std::string::npos, openPos);
    VERIFY_ARE_NOT_EQUAL(std::string::npos, closePos);
    for (size_t i = openPos; i < closePos; ++i)
    {
        if (contents[i] == '\n')
        {
            VERIFY_IS_TRUE(i > 0 && contents[i - 1] == '\r', L"Bare LF inside block — CRLF style was lost");
        }
    }
}

void ShellIntegrationTests::Install_PreservesLfFromExistingProfile()
{
    const auto profile = _ProfilePath();
    _WriteFile(profile, "Write-Host 'hi'\n");

    VERIFY_IS_TRUE(Install(profile.wstring()).success);

    const auto contents = _ReadFile(profile);
    VERIFY_IS_FALSE(_Contains(contents, "\r\n"), L"LF-only file must not gain CRLF");
}

void ShellIntegrationTests::Install_AppendsEolWhenProfileMissingTrailingNewline()
{
    const auto profile = _ProfilePath();
    _WriteFile(profile, "Write-Host 'no trailing newline'"); // no \n at end

    VERIFY_IS_TRUE(Install(profile.wstring()).success);

    const auto contents = _ReadFile(profile);
    // The original content should be followed by an EOL before the block.
    const auto blockPos = contents.find(kShellIntegrationBlockOpenMarker);
    VERIFY_ARE_NOT_EQUAL(std::string::npos, blockPos);
    VERIFY_IS_TRUE(blockPos > 0);
    VERIFY_ARE_EQUAL('\n', contents[blockPos - 1]);
}

void ShellIntegrationTests::Install_IdempotentWhenAlreadyInstalled()
{
    const auto profile = _ProfilePath();
    VERIFY_IS_TRUE(Install(profile.wstring()).success);

    const auto firstContents = _ReadFile(profile);
    const auto r2 = Install(profile.wstring());
    VERIFY_IS_TRUE(r2.success);
    VERIFY_IS_TRUE(r2.alreadyInstalled);
    VERIFY_ARE_EQUAL(firstContents, _ReadFile(profile));
}

void ShellIntegrationTests::Install_ReinstallsWhenScriptMissingButBlockMatches()
{
    const auto profile = _ProfilePath();
    VERIFY_IS_TRUE(Install(profile.wstring()).success);

    const auto scriptPath = profile.parent_path() / ShellIntegrationScriptFileName();
    std::error_code ec;
    std::filesystem::remove(scriptPath, ec);
    VERIFY_IS_FALSE(std::filesystem::exists(scriptPath));

    const auto r = Install(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled, L"Script file went missing → must re-install, not no-op");
    VERIFY_IS_TRUE(std::filesystem::exists(scriptPath));
}

void ShellIntegrationTests::Install_RewritesLegacyDotSourceLineInPlace()
{
    const auto profile = _ProfilePath();
    const std::string original =
        "Set-Alias ll Get-ChildItem\n"
        ". \"C:\\old\\path\\shell-integration_v0.ps1\"\n"
        "Write-Host 'tail'\n";
    _WriteFile(profile, original);

    const auto r = Install(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);

    const auto contents = _ReadFile(profile);
    VERIFY_IS_FALSE(_Contains(contents, "C:\\old\\path"), L"Legacy dot-source line must be replaced");
    VERIFY_IS_TRUE(_Contains(contents, kShellIntegrationBlockOpenMarker));
    VERIFY_IS_TRUE(_Contains(contents, "Set-Alias ll"), L"Surrounding user content preserved");
    VERIFY_IS_TRUE(_Contains(contents, "Write-Host 'tail'"), L"Trailing user content preserved");
    // The block should be in the middle, not at the end of the file.
    const auto blockPos = contents.find(kShellIntegrationBlockOpenMarker);
    const auto tailPos = contents.find("Write-Host 'tail'");
    VERIFY_IS_TRUE(blockPos < tailPos, L"In-place rewrite — block stays where legacy line was");
}

void ShellIntegrationTests::Install_OverwritesOrphanOpenMarker()
{
    const auto profile = _ProfilePath();
    std::string original = "Write-Host 'pre'\n";
    original += std::string{ kShellIntegrationBlockOpenMarker };
    original += "\n# Auto-generated by Intelligent Terminal. Do not edit between markers.";
    original += "\n$__it_si = 'leaked'";
    original += "\nif (Test-Path -LiteralPath $__it_si) { . $__it_si }";
    original += "\nRemove-Variable __it_si -ErrorAction SilentlyContinue\n";
    _WriteFile(profile, original);

    const auto r = Install(profile.wstring());
    VERIFY_IS_TRUE(r.success);

    const auto contents = _ReadFile(profile);
    // After install there must be exactly one open marker AND one close marker.
    size_t openCount = 0, closeCount = 0, pos = 0;
    while ((pos = contents.find(kShellIntegrationBlockOpenMarker, pos)) != std::string::npos)
    {
        ++openCount;
        pos += kShellIntegrationBlockOpenMarker.size();
    }
    pos = 0;
    while ((pos = contents.find(kShellIntegrationBlockCloseMarker, pos)) != std::string::npos)
    {
        ++closeCount;
        pos += kShellIntegrationBlockCloseMarker.size();
    }
    VERIFY_ARE_EQUAL(static_cast<size_t>(1), openCount);
    VERIFY_ARE_EQUAL(static_cast<size_t>(1), closeCount);
    // The leaked `$__it_si = 'leaked'` body line from the corrupted block
    // must NOT survive: orphan-body consumption guarantees the next
    // Install replaces the entire corrupted region, not just the open
    // marker line.
    VERIFY_IS_FALSE(_Contains(contents, "$__it_si = 'leaked'"),
                    L"Orphaned body line must be replaced by Install");
}

void ShellIntegrationTests::Install_CreatesBackupForNonEmptyProfile()
{
    const auto profile = _ProfilePath();
    _WriteFile(profile, "Write-Host 'hi'\n");

    VERIFY_IS_TRUE(Install(profile.wstring()).success);
    VERIFY_IS_GREATER_THAN_OR_EQUAL(_CountBackups(profile), static_cast<size_t>(1));
}

void ShellIntegrationTests::Install_DoesNotCreateBackupForEmptyProfile()
{
    const auto profile = _ProfilePath();
    // Profile-missing case: Install touches an empty file, then sees empty
    // contents and skips the backup (the "if (!contents.empty())" guard).
    VERIFY_IS_TRUE(Install(profile.wstring()).success);
    VERIFY_ARE_EQUAL(static_cast<size_t>(0), _CountBackups(profile));
}

void ShellIntegrationTests::Install_TwoConsecutiveCalls_AreIdempotent()
{
    const auto profile = _ProfilePath();
    VERIFY_IS_TRUE(Install(profile.wstring()).success);
    const auto firstContents = _ReadFile(profile);

    const auto r2 = Install(profile.wstring());
    VERIFY_IS_TRUE(r2.success);
    VERIFY_IS_TRUE(r2.alreadyInstalled);
    VERIFY_ARE_EQUAL(firstContents, _ReadFile(profile), L"Idempotent install must not rewrite the file");
}

// ─── Uninstall ────────────────────────────────────────────────────────────────

void ShellIntegrationTests::Uninstall_EmptyPath_Fails()
{
    const auto r = Uninstall(L"");
    VERIFY_IS_FALSE(r.success);
}

void ShellIntegrationTests::Uninstall_ProfileMissing_NoOp()
{
    const auto profile = _ProfilePath();
    VERIFY_IS_FALSE(std::filesystem::exists(profile));

    const auto r = Uninstall(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_TRUE(r.alreadyInstalled);
    VERIFY_IS_FALSE(std::filesystem::exists(profile), L"Uninstall must NOT create the profile");
}

void ShellIntegrationTests::Uninstall_ProfileWithoutBlock_NoOp()
{
    const auto profile = _ProfilePath();
    const std::string original = "Write-Host 'hi'\n";
    _WriteFile(profile, original);

    const auto r = Uninstall(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_TRUE(r.alreadyInstalled);
    VERIFY_ARE_EQUAL(original, _ReadFile(profile));
}

void ShellIntegrationTests::Uninstall_StripsModernBlockCleanly()
{
    const auto profile = _ProfilePath();
    VERIFY_IS_TRUE(Install(profile.wstring()).success);
    VERIFY_IS_TRUE(_Contains(_ReadFile(profile), kShellIntegrationBlockOpenMarker));

    const auto r = Uninstall(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);

    const auto contents = _ReadFile(profile);
    VERIFY_IS_FALSE(_Contains(contents, kShellIntegrationBlockOpenMarker));
    VERIFY_IS_FALSE(_Contains(contents, kShellIntegrationBlockCloseMarker));
    VERIFY_IS_FALSE(_Contains(contents, "$__it_si"));
}

void ShellIntegrationTests::Uninstall_StripsBlockInMiddleOfFile()
{
    const auto profile = _ProfilePath();
    const std::string pre = "Write-Host 'pre'\n";
    const std::string post = "Write-Host 'post'\n";

    std::string content = pre;
    content += std::string{ kShellIntegrationBlockOpenMarker };
    content += "\nbody\nmore body\n";
    content += std::string{ kShellIntegrationBlockCloseMarker };
    content += "\n" + post;
    _WriteFile(profile, content);

    VERIFY_IS_TRUE(Uninstall(profile.wstring()).success);

    const auto after = _ReadFile(profile);
    VERIFY_ARE_EQUAL(pre + post, after, L"Surrounding content preserved, block + its trailing newline removed");
}

void ShellIntegrationTests::Uninstall_StripsLegacyDotSourceLine()
{
    const auto profile = _ProfilePath();
    const std::string original =
        "Set-Alias ll Get-ChildItem\n"
        ". \"C:\\old\\path\\shell-integration_v0.ps1\"\n"
        "Write-Host 'tail'\n";
    _WriteFile(profile, original);

    const auto r = Uninstall(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);

    const auto contents = _ReadFile(profile);
    VERIFY_ARE_EQUAL(std::string{ "Set-Alias ll Get-ChildItem\nWrite-Host 'tail'\n" }, contents);
}

void ShellIntegrationTests::Uninstall_StripsOrphanOpenMarkerAndRecognizableBody()
{
    const auto profile = _ProfilePath();
    std::string content = "Write-Host 'pre'\n";
    content += std::string{ kShellIntegrationBlockOpenMarker };
    content += "\n# Auto-generated by Intelligent Terminal. Do not edit between markers.";
    content += "\n$__it_si = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'x.ps1'";
    content += "\nif (Test-Path -LiteralPath $__it_si) { . $__it_si }";
    content += "\nRemove-Variable __it_si -ErrorAction SilentlyContinue\n";
    _WriteFile(profile, content);

    const auto r = Uninstall(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled, L"Orphan + body must be stripped, not skipped");

    const auto remaining = _ReadFile(profile);
    VERIFY_IS_FALSE(_Contains(remaining, kShellIntegrationBlockOpenMarker),
                    L"Orphan open marker must be removed");
    VERIFY_IS_FALSE(_Contains(remaining, "$__it_si"),
                    L"Recognizable body lines must be removed");
    VERIFY_IS_TRUE(_Contains(remaining, "Write-Host 'pre'"),
                   L"User content above the orphan must be preserved");
}

void ShellIntegrationTests::Uninstall_LeavesUnrelatedTailAfterOrphanCleanup()
{
    // Orphan body followed immediately by user content: Uninstall must
    // strip ONLY the recognizable orphan region and preserve the user's
    // unrelated lines verbatim.
    const auto profile = _ProfilePath();
    std::string content = "Write-Host 'pre'\n";
    content += std::string{ kShellIntegrationBlockOpenMarker };
    content += "\n$__it_si = 'leaked'\n";
    content += "Set-Alias ll Get-ChildItem\nWrite-Host 'tail'\n";
    _WriteFile(profile, content);

    const auto r = Uninstall(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);

    const auto remaining = _ReadFile(profile);
    VERIFY_ARE_EQUAL(std::string{ "Write-Host 'pre'\nSet-Alias ll Get-ChildItem\nWrite-Host 'tail'\n" }, remaining);
}

void ShellIntegrationTests::Uninstall_CreatesBackupBeforeMutating()
{
    const auto profile = _ProfilePath();
    VERIFY_IS_TRUE(Install(profile.wstring()).success);
    const auto installBackups = _CountBackups(profile);

    VERIFY_IS_TRUE(Uninstall(profile.wstring()).success);
    VERIFY_IS_GREATER_THAN(_CountBackups(profile), installBackups);
}

void ShellIntegrationTests::Uninstall_AfterInstall_RestoresOriginalContent()
{
    const auto profile = _ProfilePath();
    const std::string original = "Set-Alias ll Get-ChildItem\nWrite-Host 'hi'\n";
    _WriteFile(profile, original);

    VERIFY_IS_TRUE(Install(profile.wstring()).success);
    VERIFY_IS_TRUE(Uninstall(profile.wstring()).success);

    // Install added a `\n` + block + `\n`; Uninstall strips block + 1 trailing eol.
    // The first appended `\n` is part of the original content's missing trailing
    // newline, so when the original DOES end with `\n`, we end up exactly back at
    // `original`. (When it doesn't, we'd end up with a `\n` added — but our
    // original here ends with `\n`, so the round trip is exact.)
    VERIFY_ARE_EQUAL(original, _ReadFile(profile));
}

void ShellIntegrationTests::Uninstall_TwoConsecutiveCalls_AreIdempotent()
{
    const auto profile = _ProfilePath();
    VERIFY_IS_TRUE(Install(profile.wstring()).success);

    const auto r1 = Uninstall(profile.wstring());
    VERIFY_IS_TRUE(r1.success);
    VERIFY_IS_FALSE(r1.alreadyInstalled);

    const auto firstContents = _ReadFile(profile);

    const auto r2 = Uninstall(profile.wstring());
    VERIFY_IS_TRUE(r2.success);
    VERIFY_IS_TRUE(r2.alreadyInstalled, L"Second uninstall should be a no-op");
    VERIFY_ARE_EQUAL(firstContents, _ReadFile(profile));
}

// ─── Round-trip ───────────────────────────────────────────────────────────────

void ShellIntegrationTests::InstallUninstallInstall_RoundTrip()
{
    const auto profile = _ProfilePath();
    const std::string original = "Write-Host 'hi'\n";
    _WriteFile(profile, original);

    VERIFY_IS_TRUE(Install(profile.wstring()).success);
    const auto afterFirstInstall = _ReadFile(profile);

    VERIFY_IS_TRUE(Uninstall(profile.wstring()).success);
    VERIFY_ARE_EQUAL(original, _ReadFile(profile));

    VERIFY_IS_TRUE(Install(profile.wstring()).success);
    VERIFY_ARE_EQUAL(afterFirstInstall, _ReadFile(profile),
                     L"Round-trip: second Install must produce byte-identical output to first");
}

// ─── ExecutionPolicy detection ────────────────────────────────────────────────

void ShellIntegrationTests::PolicyName_RestrictedAndAllSigned_AreBlocking()
{
    // The two policy names that refuse to run unsigned local scripts —
    // the exact case our $PROFILE block hits because we don't Authenticode-sign
    // it. Comparison must be lowercase (QueryExecutionPolicy normalizes its
    // output) — verifying mixed case here would test the wrong contract.
    VERIFY_IS_TRUE(details::PolicyNameBlocksUnsignedScripts(L"restricted"));
    VERIFY_IS_TRUE(details::PolicyNameBlocksUnsignedScripts(L"allsigned"));
}

void ShellIntegrationTests::PolicyName_RemoteSignedAndPermissive_AreNotBlocking()
{
    // RemoteSigned lets *local* unsigned scripts run (it only blocks
    // downloaded ones) — that's the default for pwsh on Windows, so it
    // must not trigger the EP-blocked path or we'd false-positive on
    // the most common pwsh install.
    VERIFY_IS_FALSE(details::PolicyNameBlocksUnsignedScripts(L"remote" L"signed"));
    VERIFY_IS_FALSE(details::PolicyNameBlocksUnsignedScripts(L"unrestricted"));
    VERIFY_IS_FALSE(details::PolicyNameBlocksUnsignedScripts(L"bypass"));
    VERIFY_IS_FALSE(details::PolicyNameBlocksUnsignedScripts(L"undefined"));
}

void ShellIntegrationTests::PolicyName_EmptyOrUnknown_NotBlocking()
{
    // Empty string is what QueryExecutionPolicy returns when CreateProcess
    // fails (e.g. pwsh.exe not installed). Treating that as "not blocking"
    // is the deliberate fail-open behavior: we don't want a missing
    // optional host to lock the user out of error detection.
    VERIFY_IS_FALSE(details::PolicyNameBlocksUnsignedScripts(L""));
    VERIFY_IS_FALSE(details::PolicyNameBlocksUnsignedScripts(L"something" L"else"));
}

void ShellIntegrationTests::QueryExecutionPolicy_NonexistentExe_ReturnsEmpty()
{
    // CreateProcess fails synchronously when the exe doesn't resolve —
    // QueryExecutionPolicy must return empty (not hang, not throw) so the
    // pwsh-not-installed case fails open.
    const auto out = details::QueryExecutionPolicy(L"definitely-not-a-real-binary-zzzzz.exe");
    VERIFY_IS_TRUE(out.empty());
}

void ShellIntegrationTests::QueryExecutionPolicy_ParsesStdoutAndLowercases()
{
    // Smoke test against real powershell.exe (always present on Windows).
    // We don't care WHICH policy the runner returns — we care that the
    // QueryExecutionPolicy contract holds:
    //   * the call completes within the 5s timeout (no hang on the pipe),
    //   * stdout is captured (non-empty), and
    //   * the result is lowercase ASCII letters only — the parser strips
    //     newlines / spaces / tabs, lowercases A-Z, but a stray BOM byte or
    //     control char would slip through and silently break the comparison
    //     against the known policy names in PolicyNameBlocksUnsignedScripts.
    const auto out = details::QueryExecutionPolicy(L"powershell.exe");
    VERIFY_IS_FALSE(out.empty(), L"powershell.exe must be on PATH on Windows runners");
    for (const auto c : out)
    {
        VERIFY_IS_TRUE(c >= L'a' && c <= L'z',
                       L"QueryExecutionPolicy output must be lowercase ASCII letters only "
                       L"(no whitespace, control chars, or BOM bytes leaking through)");
    }
}

void ShellIntegrationTests::QueryExecutionPolicy_TrimsWhitespaceAndStopsAtFirstLine()
{
    // Same invariant tested two ways for redundancy with the smoke test:
    // even if PowerShell ever adds blank-line padding, the parser must
    // skip leading blanks and stop at the first non-empty line. The smoke
    // test above already exercises the "single token" path; verify here that
    // calling QueryExecutionPolicy back-to-back stays cheap and consistent,
    // and that nothing depends on first-call side effects in the function.
    const auto first = details::QueryExecutionPolicy(L"powershell.exe");
    const auto second = details::QueryExecutionPolicy(L"powershell.exe");
    VERIFY_ARE_EQUAL(first, second, L"QueryExecutionPolicy must be deterministic for the same host");
}

// ═════════════════════════════════════════════════════════════════════════════
// Bash flavor
// ═════════════════════════════════════════════════════════════════════════════

// ─── FindShellIntegrationBashBlock ────────────────────────────────────────────

void ShellIntegrationTests::Bash_FindBlock_EmptyContent_ReturnsNpos()
{
    const auto [s, e] = FindShellIntegrationBashBlock("");
    VERIFY_ARE_EQUAL(std::string::npos, s);
    VERIFY_ARE_EQUAL(std::string::npos, e);
}

void ShellIntegrationTests::Bash_FindBlock_UnrelatedContent_ReturnsNpos()
{
    const auto [s, e] = FindShellIntegrationBashBlock("export PATH=$PATH:/usr/local/bin\nalias ll='ls -la'\n");
    VERIFY_ARE_EQUAL(std::string::npos, s);
    VERIFY_ARE_EQUAL(std::string::npos, e);
}

void ShellIntegrationTests::Bash_FindBlock_ModernBlock_ReturnsRange()
{
    std::string content = "alias ll='ls -la'\n";
    const auto blockStart = content.size();
    content += std::string{ kShellIntegrationBlockOpenMarker };
    content += "\nbody\n";
    content += std::string{ kShellIntegrationBlockCloseMarker };
    const auto blockEnd = content.size();
    content += "\nexport FOO=bar\n";

    const auto [s, e] = FindShellIntegrationBashBlock(content);
    VERIFY_ARE_EQUAL(blockStart, s);
    VERIFY_ARE_EQUAL(blockEnd, e);
}

void ShellIntegrationTests::Bash_FindBlock_OrphanOpenMarker_ConsumesRecognizableBodyLines()
{
    // Open marker present, no close marker, but recognizable body lines.
    // Find must extend past the marker line through the recognized body.
    std::string content = "alias ll='ls -la'\n";
    const auto start = content.size();
    content += std::string{ kShellIntegrationBlockOpenMarker };
    content += "\n# Auto-generated by Intelligent Terminal. Do not edit between markers.";
    content += "\nif [ -n \"$BASH_VERSION\" ]; then";
    content += "\n    __it_si=\"$HOME/.intelligent-terminal/shell-integration_v1.sh\"";
    content += "\n    [ -f \"$__it_si\" ] && . \"$__it_si\"";
    content += "\n    unset __it_si";
    content += "\nfi";

    const auto [s, e] = FindShellIntegrationBashBlock(content);
    VERIFY_ARE_EQUAL(start, s);
    VERIFY_ARE_EQUAL(content.size(), e);
}

void ShellIntegrationTests::Bash_FindBlock_OrphanOpenMarker_StopsAtUnrelatedUserContent()
{
    // Stop at first non-recognized line so user content below the
    // corruption is preserved.
    std::string content;
    content += std::string{ kShellIntegrationBlockOpenMarker };
    content += "\n# Auto-generated by Intelligent Terminal. Do not edit between markers.";
    const auto expectedEnd = content.size();
    content += "\necho 'this is user content, must survive'";
    content += "\nexport USER_THING=1";

    const auto [s, e] = FindShellIntegrationBashBlock(content);
    VERIFY_ARE_EQUAL(static_cast<size_t>(0), s);
    VERIFY_ARE_EQUAL(expectedEnd, e);
}

// ─── BuildShellIntegrationBashBlock + script content ──────────────────────────

void ShellIntegrationTests::Bash_BuildBlock_ContainsMarkersAndScriptFilename()
{
    const auto block = BuildShellIntegrationBashBlock();
    VERIFY_IS_TRUE(_Contains(block, kShellIntegrationBlockOpenMarker));
    VERIFY_IS_TRUE(_Contains(block, kShellIntegrationBlockCloseMarker));
    VERIFY_IS_TRUE(_Contains(block,
                             "shell-integration_v" + std::to_string(kShellIntegrationBashVersion) + ".sh"));
}

void ShellIntegrationTests::Bash_BuildBlock_IsLfOnly()
{
    const auto block = BuildShellIntegrationBashBlock();
    VERIFY_IS_FALSE(_Contains(block, "\r\n"),
                    L"Bash block must be LF-only — bash files are never CRLF");
    VERIFY_IS_FALSE(_Contains(block, "\r"),
                    L"Bash block must not contain bare CR either");
}

void ShellIntegrationTests::Bash_BuildBlock_UsesHomeAndGuardsOnBashVersion()
{
    const auto block = BuildShellIntegrationBashBlock();
    // Machine-portable: references $HOME, not a hardcoded path. This is
    // the bash analogue of the PS block's runtime Documents resolution
    // and is the property that lets .bashrc roam across machines safely.
    VERIFY_IS_TRUE(_Contains(block, "$HOME/"));
    VERIFY_IS_FALSE(_Contains(block, "C:\\"),
                    L"Block must NOT contain a hardcoded Windows path");
    // Bash-only guard so the block is a silent no-op when .bashrc is
    // sourced by sh / dash / zsh.
    VERIFY_IS_TRUE(_Contains(block, "$BASH_VERSION"));
    // Missing-script guard so roaming to a machine without IT installed
    // is a silent no-op rather than an error per shell start.
    VERIFY_IS_TRUE(_Contains(block, "[ -f \"$__it_si\" ]"));
}

void ShellIntegrationTests::Bash_ScriptContent_HasIdempotencyGuardAndOscSequences()
{
    const auto& script = ShellIntegrationBashScriptContent();

    // Idempotency: must guard against double-sourcing.
    VERIFY_IS_TRUE(_Contains(script, "__IT_SHELLINTEG_INSTALLED"));
    // Bash-only + interactive-only guards.
    VERIFY_IS_TRUE(_Contains(script, "BASH_VERSION"));
    VERIFY_IS_TRUE(_Contains(script, "case \"${-:-}\" in *i*"));
    // The three OSC sequences the autofix pipeline downstream depends on.
    VERIFY_IS_TRUE(_Contains(script, "133;D;%s"));
    VERIFY_IS_TRUE(_Contains(script, "133;A"));
    VERIFY_IS_TRUE(_Contains(script, "133;B"));
    // CWD reporting.
    VERIFY_IS_TRUE(_Contains(script, "9;9;"));
    // Preserves the user's existing PROMPT_COMMAND.
    VERIFY_IS_TRUE(_Contains(script, "__IT_SHELLINTEG_USER_PC"));
    // Preserves $? for that user hook so its `local ec=$?` still works.
    VERIFY_IS_TRUE(_Contains(script, "(exit \"$__ec\")"));
    // `set -u` (nounset) safety: every variable that might be unset
    // BEFORE we touch it must use ${VAR:-} defaulting. A user with
    // `set -u` earlier in .bashrc must not see "unbound variable" noise
    // from sourcing our script.
    VERIFY_IS_TRUE(_Contains(script, "${BASH_VERSION:-}"));
    VERIFY_IS_TRUE(_Contains(script, "${-:-}"));
    VERIFY_IS_TRUE(_Contains(script, "${__IT_SHELLINTEG_INSTALLED:-}"));
    VERIFY_IS_TRUE(_Contains(script, "${PROMPT_COMMAND:-}"));
    VERIFY_IS_TRUE(_Contains(script, "${PS1:-}"));
}

// ─── InstallBash ──────────────────────────────────────────────────────────────

void ShellIntegrationTests::Bash_Install_EmptyProfilePath_Fails()
{
    const auto r = InstallBash(L"", _BashScriptDir().wstring());
    VERIFY_IS_FALSE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);
    VERIFY_IS_FALSE(r.errorMessage.empty());
}

void ShellIntegrationTests::Bash_Install_EmptyScriptDir_Fails()
{
    const auto r = InstallBash(_BashProfilePath().wstring(), L"");
    VERIFY_IS_FALSE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);
    VERIFY_IS_FALSE(r.errorMessage.empty());
}

void ShellIntegrationTests::Bash_Install_ProfileMissing_CreatesProfileAndScript()
{
    const auto profile = _BashProfilePath();
    const auto scriptDir = _BashScriptDir();
    VERIFY_IS_FALSE(std::filesystem::exists(profile));

    const auto r = InstallBash(profile.wstring(), scriptDir.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);
    VERIFY_IS_TRUE(std::filesystem::exists(profile));
    VERIFY_IS_TRUE(std::filesystem::exists(scriptDir / ShellIntegrationBashScriptFileName()));

    const auto contents = _ReadFile(profile);
    VERIFY_IS_TRUE(_Contains(contents, kShellIntegrationBlockOpenMarker));
    VERIFY_IS_TRUE(_Contains(contents, kShellIntegrationBlockCloseMarker));
}

void ShellIntegrationTests::Bash_Install_ProfileWithoutBlock_AppendsBlockPreservesOriginalContent()
{
    const auto profile = _BashProfilePath();
    const std::string original = "export PATH=$PATH:/usr/local/bin\nalias ll='ls -la'\n";
    _WriteFile(profile, original);

    const auto r = InstallBash(profile.wstring(), _BashScriptDir().wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);

    const auto contents = _ReadFile(profile);
    VERIFY_IS_TRUE(contents.rfind(original, 0) == 0, L"Original content must remain at start of .bashrc");
    VERIFY_IS_TRUE(_Contains(contents, kShellIntegrationBlockOpenMarker));
}

void ShellIntegrationTests::Bash_Install_IsLfOnly()
{
    const auto profile = _BashProfilePath();
    // Even if a user (or a buggy editor) introduced CRLF, our install
    // must not emit CRLF inside its own block — bash tolerates both,
    // but our block style stays consistent with the bash convention.
    _WriteFile(profile, "alias ll='ls -la'\r\n");

    VERIFY_IS_TRUE(InstallBash(profile.wstring(), _BashScriptDir().wstring()).success);

    const auto contents = _ReadFile(profile);
    const auto openPos = contents.find(kShellIntegrationBlockOpenMarker);
    const auto closePos = contents.find(kShellIntegrationBlockCloseMarker, openPos);
    VERIFY_ARE_NOT_EQUAL(std::string::npos, openPos);
    VERIFY_ARE_NOT_EQUAL(std::string::npos, closePos);
    for (size_t i = openPos; i < closePos; ++i)
    {
        VERIFY_ARE_NOT_EQUAL('\r', contents[i],
                             L"Bash block must contain no CR characters");
    }
}

void ShellIntegrationTests::Bash_Install_IdempotentWhenAlreadyInstalled()
{
    const auto profile = _BashProfilePath();
    const auto scriptDir = _BashScriptDir();
    VERIFY_IS_TRUE(InstallBash(profile.wstring(), scriptDir.wstring()).success);

    const auto firstContents = _ReadFile(profile);
    const auto r2 = InstallBash(profile.wstring(), scriptDir.wstring());
    VERIFY_IS_TRUE(r2.success);
    VERIFY_IS_TRUE(r2.alreadyInstalled);
    VERIFY_ARE_EQUAL(firstContents, _ReadFile(profile));
}

void ShellIntegrationTests::Bash_Install_ReinstallsWhenScriptMissingButBlockMatches()
{
    const auto profile = _BashProfilePath();
    const auto scriptDir = _BashScriptDir();
    VERIFY_IS_TRUE(InstallBash(profile.wstring(), scriptDir.wstring()).success);

    const auto scriptPath = scriptDir / ShellIntegrationBashScriptFileName();
    std::error_code ec;
    std::filesystem::remove(scriptPath, ec);
    VERIFY_IS_FALSE(std::filesystem::exists(scriptPath));

    const auto r = InstallBash(profile.wstring(), scriptDir.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled, L"Script file went missing → must re-install, not no-op");
    VERIFY_IS_TRUE(std::filesystem::exists(scriptPath));
}

void ShellIntegrationTests::Bash_Install_OverwritesOrphanOpenMarker()
{
    const auto profile = _BashProfilePath();
    std::string original = "alias ll='ls -la'\n";
    original += std::string{ kShellIntegrationBlockOpenMarker };
    original += "\n# Auto-generated by Intelligent Terminal. Do not edit between markers.";
    original += "\nif [ -n \"$BASH_VERSION\" ]; then";
    original += "\n    __it_si=\"/leaked/path\"";
    original += "\n    [ -f \"$__it_si\" ] && . \"$__it_si\"";
    original += "\n    unset __it_si";
    original += "\nfi\n";
    _WriteFile(profile, original);

    const auto r = InstallBash(profile.wstring(), _BashScriptDir().wstring());
    VERIFY_IS_TRUE(r.success);

    const auto contents = _ReadFile(profile);
    size_t openCount = 0, closeCount = 0, pos = 0;
    while ((pos = contents.find(kShellIntegrationBlockOpenMarker, pos)) != std::string::npos)
    {
        ++openCount;
        pos += kShellIntegrationBlockOpenMarker.size();
    }
    pos = 0;
    while ((pos = contents.find(kShellIntegrationBlockCloseMarker, pos)) != std::string::npos)
    {
        ++closeCount;
        pos += kShellIntegrationBlockCloseMarker.size();
    }
    VERIFY_ARE_EQUAL(static_cast<size_t>(1), openCount);
    VERIFY_ARE_EQUAL(static_cast<size_t>(1), closeCount);
    VERIFY_IS_FALSE(_Contains(contents, "/leaked/path"),
                    L"Orphaned body line must be replaced by InstallBash");
}

void ShellIntegrationTests::Bash_Install_CreatesBackupForNonEmptyProfile()
{
    const auto profile = _BashProfilePath();
    _WriteFile(profile, "alias ll='ls -la'\n");

    VERIFY_IS_TRUE(InstallBash(profile.wstring(), _BashScriptDir().wstring()).success);
    VERIFY_IS_GREATER_THAN_OR_EQUAL(_CountBackups(profile), static_cast<size_t>(1));
}

void ShellIntegrationTests::Bash_Install_DoesNotCreateBackupForEmptyProfile()
{
    const auto profile = _BashProfilePath();
    VERIFY_IS_TRUE(InstallBash(profile.wstring(), _BashScriptDir().wstring()).success);
    VERIFY_ARE_EQUAL(static_cast<size_t>(0), _CountBackups(profile));
}

// ─── UninstallBash ────────────────────────────────────────────────────────────

void ShellIntegrationTests::Bash_Uninstall_EmptyPath_Fails()
{
    const auto r = UninstallBash(L"");
    VERIFY_IS_FALSE(r.success);
}

void ShellIntegrationTests::Bash_Uninstall_ProfileMissing_NoOp()
{
    const auto profile = _BashProfilePath();
    VERIFY_IS_FALSE(std::filesystem::exists(profile));

    const auto r = UninstallBash(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_TRUE(r.alreadyInstalled);
    VERIFY_IS_FALSE(std::filesystem::exists(profile), L"UninstallBash must NOT create .bashrc");
}

void ShellIntegrationTests::Bash_Uninstall_ProfileWithoutBlock_NoOp()
{
    const auto profile = _BashProfilePath();
    const std::string original = "alias ll='ls -la'\n";
    _WriteFile(profile, original);

    const auto r = UninstallBash(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_TRUE(r.alreadyInstalled);
    VERIFY_ARE_EQUAL(original, _ReadFile(profile));
}

void ShellIntegrationTests::Bash_Uninstall_StripsBlockCleanly()
{
    const auto profile = _BashProfilePath();
    const auto scriptDir = _BashScriptDir();
    _WriteFile(profile, "alias ll='ls -la'\nexport FOO=bar\n");

    VERIFY_IS_TRUE(InstallBash(profile.wstring(), scriptDir.wstring()).success);

    const auto r = UninstallBash(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);

    const auto contents = _ReadFile(profile);
    VERIFY_IS_FALSE(_Contains(contents, kShellIntegrationBlockOpenMarker));
    VERIFY_IS_FALSE(_Contains(contents, kShellIntegrationBlockCloseMarker));
    VERIFY_IS_TRUE(_Contains(contents, "alias ll='ls -la'"));
    VERIFY_IS_TRUE(_Contains(contents, "export FOO=bar"));
}

void ShellIntegrationTests::Bash_Uninstall_AfterInstall_RestoresOriginalContent()
{
    const auto profile = _BashProfilePath();
    const std::string original = "alias ll='ls -la'\nexport FOO=bar\n";
    _WriteFile(profile, original);

    VERIFY_IS_TRUE(InstallBash(profile.wstring(), _BashScriptDir().wstring()).success);
    VERIFY_IS_TRUE(UninstallBash(profile.wstring()).success);

    VERIFY_ARE_EQUAL(original, _ReadFile(profile));
}

void ShellIntegrationTests::Bash_Uninstall_TwoConsecutiveCalls_AreIdempotent()
{
    const auto profile = _BashProfilePath();
    VERIFY_IS_TRUE(InstallBash(profile.wstring(), _BashScriptDir().wstring()).success);

    const auto r1 = UninstallBash(profile.wstring());
    VERIFY_IS_TRUE(r1.success);
    VERIFY_IS_FALSE(r1.alreadyInstalled);

    const auto r2 = UninstallBash(profile.wstring());
    VERIFY_IS_TRUE(r2.success);
    VERIFY_IS_TRUE(r2.alreadyInstalled, L"Second uninstall must be a no-op");
}

void ShellIntegrationTests::Bash_InstallUninstallInstall_RoundTrip()
{
    const auto profile = _BashProfilePath();
    const auto scriptDir = _BashScriptDir();
    _WriteFile(profile, "alias ll='ls -la'\n");

    VERIFY_IS_TRUE(InstallBash(profile.wstring(), scriptDir.wstring()).success);
    const auto afterFirstInstall = _ReadFile(profile);

    VERIFY_IS_TRUE(UninstallBash(profile.wstring()).success);
    VERIFY_IS_TRUE(InstallBash(profile.wstring(), scriptDir.wstring()).success);

    VERIFY_ARE_EQUAL(afterFirstInstall, _ReadFile(profile),
                     L"Round-trip: second Install must produce byte-identical output to first");
}

// ═════════════════════════════════════════════════════════════════════════════
// WSL flavor
//
// Install/UninstallWslBash require a real running WSL distro on the host —
// we cover only the pure-function helpers here. The shared UNC-mediated
// write path is already covered by the Bash_* tests; once QueryWslHomeRaw
// returns successfully the implementation IS InstallBash / UninstallBash
// with a different profilePath / scriptDir.
// ═════════════════════════════════════════════════════════════════════════════

void ShellIntegrationTests::Wsl_IsSafeDistroName_AcceptsCommonNames()
{
    VERIFY_IS_TRUE(details::IsSafeWslDistroName(L"Ubuntu"));
    VERIFY_IS_TRUE(details::IsSafeWslDistroName(L"Ubuntu-22.04"));
    VERIFY_IS_TRUE(details::IsSafeWslDistroName(L"Ubuntu-18.04"));
    VERIFY_IS_TRUE(details::IsSafeWslDistroName(L"Debian"));
    VERIFY_IS_TRUE(details::IsSafeWslDistroName(L"kali-linux"));
    VERIFY_IS_TRUE(details::IsSafeWslDistroName(L"openSUSE-Tumbleweed"));
    VERIFY_IS_TRUE(details::IsSafeWslDistroName(L"Alpine"));
    VERIFY_IS_TRUE(details::IsSafeWslDistroName(L"docker-desktop"));
    VERIFY_IS_TRUE(details::IsSafeWslDistroName(L"my_custom_distro_42"));
}

void ShellIntegrationTests::Wsl_IsSafeDistroName_RejectsInjection()
{
    // Anything that could break out of the `wsl.exe -d <name>` argument
    // boundary or pull in additional shell behavior must be rejected.
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu\""));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu\\Debian"));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu/Debian"));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu Debian"));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu;rm -rf ~"));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu&calc"));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu|cat"));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu`whoami`"));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu$HOME"));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu\nDebian"));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"Ubuntu\rDebian"));
    // wstring_view of a literal with embedded NUL needs the size to
    // include the NUL byte explicitly.
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(std::wstring_view{ L"Ubuntu\0Debian", 13 }));
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L"\u65e5\u672c\u8a9e")); // non-ASCII
}

void ShellIntegrationTests::Wsl_IsSafeDistroName_RejectsEmptyAndOverlong()
{
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(L""));
    std::wstring overlong(257, L'a');
    VERIFY_IS_FALSE(details::IsSafeWslDistroName(overlong));
}

void ShellIntegrationTests::Wsl_IsSafeWslHome_AcceptsCommonHomes()
{
    VERIFY_IS_TRUE(details::IsSafeWslHome("/home/yeelam"));
    VERIFY_IS_TRUE(details::IsSafeWslHome("/root"));
    VERIFY_IS_TRUE(details::IsSafeWslHome("/home/user.with.dots"));
    VERIFY_IS_TRUE(details::IsSafeWslHome("/home/user-name_42"));
    VERIFY_IS_TRUE(details::IsSafeWslHome("/var/lib/something/home/x"));
}

void ShellIntegrationTests::Wsl_IsSafeWslHome_RejectsRelativeAndTraversal()
{
    VERIFY_IS_FALSE(details::IsSafeWslHome(""));
    VERIFY_IS_FALSE(details::IsSafeWslHome("home/yeelam"));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/yeelam/"));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home//yeelam"));
    // Dot-only segments (current dir / traversal) — any segment whose
    // characters are entirely `.` must be rejected.
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/."));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/./home"));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/./x"));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/.."));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/../etc"));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/../etc"));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/..."));
    // But legitimate dot-containing segments (`.bashrc`, `user.name`)
    // must still pass — they are NOT dot-only segments.
    VERIFY_IS_TRUE(details::IsSafeWslHome("/home/user.name"));
    VERIFY_IS_TRUE(details::IsSafeWslHome("/home/a.b.c"));
}

void ShellIntegrationTests::Wsl_IsSafeWslHome_RejectsBadChars()
{
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/yee lam"));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/yeelam\""));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/yeelam;rm"));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/yeelam\n"));
    VERIFY_IS_FALSE(details::IsSafeWslHome("/home/\xc3\xa9")); // UTF-8 byte
}

void ShellIntegrationTests::Wsl_UncPath_BuildsExpectedFormat()
{
    VERIFY_ARE_EQUAL(std::wstring{ LR"(\\wsl$\Ubuntu/home/yeelam/.bashrc)" },
                     WslUncPath(L"Ubuntu", "/home/yeelam/.bashrc"));
    VERIFY_ARE_EQUAL(std::wstring{ LR"(\\wsl$\Debian-12/root/.intelligent-terminal)" },
                     WslUncPath(L"Debian-12", "/root/.intelligent-terminal"));
    // Forward slashes inside the posix part are preserved — the
    // \\wsl$\ provider routes the lookup through the distro's vfs.
    VERIFY_ARE_EQUAL(std::wstring{ LR"(\\wsl$\Alpine/home/x/y/z)" },
                     WslUncPath(L"Alpine", "/home/x/y/z"));
}

void ShellIntegrationTests::Wsl_InstallWslBash_RejectsUnsafeDistroName()
{
    // The validator gates the wsl.exe spawn — we must never even
    // attempt to launch with a tainted name. Verifying the early
    // return prevents a regression where someone reorders the checks.
    const auto r = InstallWslBash(L"Ubuntu\"; rm -rf ~ ; \"");
    VERIFY_IS_FALSE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);
    VERIFY_IS_FALSE(r.errorMessage.empty());
}

void ShellIntegrationTests::Wsl_UninstallWslBash_RejectsUnsafeDistroName()
{
    const auto r = UninstallWslBash(L"Ubuntu | calc");
    VERIFY_IS_FALSE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled);
    VERIFY_IS_FALSE(r.errorMessage.empty());
}
