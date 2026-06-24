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
    TEST_METHOD(Install_UpgradesWhenBlockReferencesOlderScriptVersion);
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

    // ResolvePowerShellHostInstall — pure install/EP verdict for one PS host.
    // Guards the regression where profile-gating silently skipped the
    // execution-policy block (FRE no longer stopped on a Restricted host).
    TEST_METHOD(ResolveHost_NoProfile_PolicyBlocked_StopsWithEpFlagWithoutWriting);
    TEST_METHOD(ResolveHost_NoProfile_PolicyOk_SucceedsWithoutWriting);
    TEST_METHOD(ResolveHost_Profile_PolicyOk_PerformsWrite);
    TEST_METHOD(ResolveHost_Profile_PolicyBlocked_StopsWithEpFlagWithoutWriting);

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
    TEST_METHOD(Bash_Install_UpgradesWhenBlockReferencesOlderScriptVersion);
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
    TEST_METHOD(Wsl_StripExecTail_StripsExistingExecCommand);
    TEST_METHOD(Wsl_QualifyBareLauncher_QualifiesBareWslBash);

    // Profile-presence gate (ShellIntegrationProfileGate.h)
    TEST_METHOD(ProfileGate_PwshSourceMatches);
    TEST_METHOD(ProfileGate_PwshCommandlineLeafExeMatches);
    TEST_METHOD(ProfileGate_WindowsPowerShellOnlyWhenNotPwsh);
    TEST_METHOD(ProfileGate_WindowsPowerShellWithDeveloperVsProfile);
    TEST_METHOD(ProfileGate_BashOnlyForGitBashNotWsl);
    TEST_METHOD(ProfileGate_BashRejectsSystem32WslLauncher);
    TEST_METHOD(ProfileGate_AnyProfileEmptyCollection);
    TEST_METHOD(ProfileGate_AnyProfileFindsOne);
    TEST_METHOD(ProfileGate_AnyProfileMissingShellReturnsFalse);

    // IsWslProfile — pure, Source-independent WSL recognizer (no parsing;
    // the installer reuses the commandline + probes $WSL_DISTRO_NAME).
    TEST_METHOD(IsWslProfile_WslLauncherForms_True);
    TEST_METHOD(IsWslProfile_System32BashLauncher_True);
    TEST_METHOD(IsWslProfile_GitBashAndOthers_False);

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

void ShellIntegrationTests::Install_UpgradesWhenBlockReferencesOlderScriptVersion()
{
    // Regression: bumping the script version (e.g. v1 -> v2 when OSC
    // 9001;ShellType emission was added) must actually reach existing
    // users. Their $PROFILE already has a well-formed managed block, but it
    // references the OLDER versioned script filename and the stale old
    // script sits on disk. The block-match early-out must NOT fire (the
    // block no longer equals the desired, current-version block), so the
    // current script is written and the block is rewritten to point at it.
    const auto profile = _ProfilePath();
    const auto currentName = til::u16u8(ShellIntegrationScriptFileName());

    // Simulate a prior install: take the current block and point it at an
    // older script version. v0 is always older than any shipped vN, so this
    // stays valid across future bumps.
    const std::string oldName = "shell-integration_v0.ps1";
    auto oldBlock = BuildShellIntegrationBlock(L"PowerShell", "\n");
    const auto namePos = oldBlock.find(currentName);
    VERIFY_ARE_NOT_EQUAL(std::string::npos, namePos, L"Block must embed the current script filename");
    oldBlock.replace(namePos, currentName.size(), oldName);
    _WriteFile(profile, oldBlock + "\n");
    // Stale old script on disk, without the ShellType emission.
    _WriteFile(profile.parent_path() / L"shell-integration_v0.ps1", "# stale old script, no ShellType\n");

    const auto r = Install(profile.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled, L"Block referenced an older script version → must upgrade, not no-op");

    const auto contents = _ReadFile(profile);
    VERIFY_IS_TRUE(_Contains(contents, currentName), L"Block must be rewritten to the current script version");
    VERIFY_IS_FALSE(_Contains(contents, oldName), L"Old version reference must be replaced");

    const auto scriptPath = profile.parent_path() / ShellIntegrationScriptFileName();
    VERIFY_IS_TRUE(std::filesystem::exists(scriptPath), L"Current-version script must be written on upgrade");
    VERIFY_IS_TRUE(_Contains(_ReadFile(scriptPath), "9001"), L"Upgraded script must emit OSC 9001;ShellType");
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
    //   * the call completes within the 20s timeout (no hang on the pipe),
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

// ─── ResolvePowerShellHostInstall ─────────────────────────────────────────────
// These cover the seam that the string-only PolicyName_* tests above cannot:
// the verdict that decides whether FRE / Save stops. The regression (PR #222)
// profile-gated this verdict so a Restricted host with no matching Windows
// Terminal profile silently reported success and FRE never stopped. The
// no-profile + blocked case below fails on that exact bug.

void ShellIntegrationTests::ResolveHost_NoProfile_PolicyBlocked_StopsWithEpFlagWithoutWriting()
{
    // The host has NO Windows Terminal profile, but its execution policy
    // blocks unsigned scripts. The verdict MUST still be a failure carrying
    // executionPolicyBlocked=true (so FRE shows the policy error + turns auto
    // detection off), and the $PROFILE write must NOT be attempted.
    int writeCalls = 0;
    const auto result = ResolvePowerShellHostInstall(
        /*profilePresent*/ false,
        /*executionPolicyBlocked*/ true,
        [&] { ++writeCalls; return InstallResult{ true, false, {}, false }; });

    VERIFY_IS_FALSE(result.success, L"A blocking policy must fail the verdict even with no profile");
    VERIFY_IS_TRUE(result.executionPolicyBlocked, L"executionPolicyBlocked must propagate so FRE shows the policy error");
    VERIFY_ARE_EQUAL(0, writeCalls, L"No $PROFILE write may be attempted when the policy blocks");
}

void ShellIntegrationTests::ResolveHost_NoProfile_PolicyOk_SucceedsWithoutWriting()
{
    // No profile and a permissive policy: nothing to do. Report
    // success-already-satisfied (so the sweep's all-installed verdict isn't
    // tripped) without writing.
    int writeCalls = 0;
    const auto result = ResolvePowerShellHostInstall(
        /*profilePresent*/ false,
        /*executionPolicyBlocked*/ false,
        [&] { ++writeCalls; return InstallResult{ true, false, {}, false }; });

    VERIFY_IS_TRUE(result.success);
    VERIFY_IS_FALSE(result.executionPolicyBlocked);
    VERIFY_ARE_EQUAL(0, writeCalls, L"No write when the user has no profile for this host");
}

void ShellIntegrationTests::ResolveHost_Profile_PolicyOk_PerformsWrite()
{
    // Profile present and policy OK: delegate to the write, returning exactly
    // what it reports.
    int writeCalls = 0;
    const auto result = ResolvePowerShellHostInstall(
        /*profilePresent*/ true,
        /*executionPolicyBlocked*/ false,
        [&] { ++writeCalls; return InstallResult{ true, false, L"sentinel", false }; });

    VERIFY_ARE_EQUAL(1, writeCalls, L"The write must run when a profile is present and the policy is fine");
    VERIFY_IS_TRUE(result.success);
    VERIFY_ARE_EQUAL(std::wstring{ L"sentinel" }, result.errorMessage, L"The write's own result must be returned verbatim");
}

void ShellIntegrationTests::ResolveHost_Profile_PolicyBlocked_StopsWithEpFlagWithoutWriting()
{
    // Profile present but policy blocks: short-circuit before writing (the
    // block would only throw PSSecurityException on every shell start anyway)
    // and surface the policy verdict.
    int writeCalls = 0;
    const auto result = ResolvePowerShellHostInstall(
        /*profilePresent*/ true,
        /*executionPolicyBlocked*/ true,
        [&] { ++writeCalls; return InstallResult{ true, false, {}, false }; });

    VERIFY_IS_FALSE(result.success);
    VERIFY_IS_TRUE(result.executionPolicyBlocked);
    VERIFY_ARE_EQUAL(0, writeCalls, L"A blocking policy must short-circuit before the write");
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
    content += "\nif [ -n \"${BASH_VERSION:-}\" ]; then";
    content += "\n    __it_si=\"${HOME:-}/.intelligent-terminal/shell-integration_v1.sh\"";
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
    VERIFY_IS_TRUE(_Contains(block, "${HOME:-}/"));
    VERIFY_IS_FALSE(_Contains(block, "C:\\"),
                    L"Block must NOT contain a hardcoded Windows path");
    // Bash-only guard so the block is a silent no-op when .bashrc is
    // sourced by sh / dash / zsh.
    VERIFY_IS_TRUE(_Contains(block, "${BASH_VERSION:-}"));
    // set -u safety: never use bare $BASH_VERSION/$HOME — sourcing under
    // `set -u` enabled earlier in .bashrc would raise "unbound variable"
    // before our guard runs.
    VERIFY_IS_FALSE(_Contains(block, "\"$BASH_VERSION\""),
                    L"Block must use ${BASH_VERSION:-} not bare $BASH_VERSION");
    VERIFY_IS_FALSE(_Contains(block, "\"$HOME/"),
                    L"Block must use ${HOME:-} not bare $HOME");
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
    // CWD reporting — unquoted form (the Terminal's 9;9 parser
    // rejects payloads with embedded quotes, and Linux paths can
    // contain `"`; the unquoted form parses cleanly regardless).
    VERIFY_IS_TRUE(_Contains(script, "9;9;%s\\007"));
    VERIFY_IS_FALSE(_Contains(script, "9;9;\"%s\""),
                    L"Script must NOT wrap the CWD payload in quotes");
    // Preserves the user's existing PROMPT_COMMAND.
    VERIFY_IS_TRUE(_Contains(script, "__IT_SHELLINTEG_USER_PC"));
    // Preserves $? for that user hook so its `local ec=$?` still works.
    VERIFY_IS_TRUE(_Contains(script, "(exit \"$__ec\")"));
    // `set -u` safety: every variable that might be unset
    // BEFORE we touch it must use ${VAR:-} defaulting. A user with
    // `set -u` earlier in .bashrc must not see "unbound variable" noise
    // from sourcing our script.
    VERIFY_IS_TRUE(_Contains(script, "${BASH_VERSION:-}"));
    VERIFY_IS_TRUE(_Contains(script, "${-:-}"));
    VERIFY_IS_TRUE(_Contains(script, "${__IT_SHELLINTEG_INSTALLED:-}"));
    // PROMPT_COMMAND can be an array (bash 5.1+) so scalar ${VAR:-}
    // defaulting is wrong for it — it would only see element [0]. The
    // set -u-safe form for the array read is the ${ARR[@]+...}
    // alternate-value guard, which collapses to nothing when unset.
    VERIFY_IS_TRUE(_Contains(script, "${PROMPT_COMMAND[@]+\"${PROMPT_COMMAND[@]}\"}"));
    VERIFY_IS_TRUE(_Contains(script, "${PS1:-}"));
    // PWD too — printf reads it for the OSC 9;9 CWD report.
    VERIFY_IS_TRUE(_Contains(script, "${PWD:-}"));
    VERIFY_IS_FALSE(_Contains(script, "\"$PWD\""),
                    L"Script must use ${PWD:-} not bare $PWD (set -u safety)");
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

void ShellIntegrationTests::Bash_Install_UpgradesWhenBlockReferencesOlderScriptVersion()
{
    // Bash/WSL counterpart of the PowerShell upgrade regression: an existing
    // ~/.bashrc managed block that references an OLDER versioned script must
    // be rewritten to the current version (so the OSC 9001;ShellType emission
    // added in v2 actually reaches users who already had v1 installed).
    const auto profile = _BashProfilePath();
    const auto scriptDir = _BashScriptDir();
    const auto currentName = til::u16u8(ShellIntegrationBashScriptFileName());

    const std::string oldName = "shell-integration_v0.sh";
    auto oldBlock = BuildShellIntegrationBashBlock();
    const auto namePos = oldBlock.find(currentName);
    VERIFY_ARE_NOT_EQUAL(std::string::npos, namePos, L"Block must embed the current script filename");
    oldBlock.replace(namePos, currentName.size(), oldName);
    _WriteFile(profile, oldBlock + "\n");
    // Stale old script on disk, without the ShellType emission.
    _WriteFile(scriptDir / L"shell-integration_v0.sh", "# stale old script, no ShellType\n");

    const auto r = InstallBash(profile.wstring(), scriptDir.wstring());
    VERIFY_IS_TRUE(r.success);
    VERIFY_IS_FALSE(r.alreadyInstalled, L"Block referenced an older script version → must upgrade, not no-op");

    const auto contents = _ReadFile(profile);
    VERIFY_IS_TRUE(_Contains(contents, currentName), L"Block must be rewritten to the current script version");
    VERIFY_IS_FALSE(_Contains(contents, oldName), L"Old version reference must be replaced");

    const auto scriptPath = scriptDir / ShellIntegrationBashScriptFileName();
    VERIFY_IS_TRUE(std::filesystem::exists(scriptPath), L"Current-version script must be written on upgrade");
    VERIFY_IS_TRUE(_Contains(_ReadFile(scriptPath), "9001"), L"Upgraded script must emit OSC 9001;ShellType");
}

void ShellIntegrationTests::Bash_Install_OverwritesOrphanOpenMarker()
{
    const auto profile = _BashProfilePath();
    std::string original = "alias ll='ls -la'\n";
    original += std::string{ kShellIntegrationBlockOpenMarker };
    original += "\n# Auto-generated by Intelligent Terminal. Do not edit between markers.";
    original += "\nif [ -n \"${BASH_VERSION:-}\" ]; then";
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
// write path is already covered by the Bash_* tests; once
// QueryWslIdentityRaw returns successfully the implementation IS
// InstallBash / UninstallBash with a different profilePath / scriptDir.
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
    // Canonical UNC form: backslash separator between distro and the
    // in-distro path, and forward slashes in the posix portion are
    // converted to backslashes. Both forms work with the Win32 file
    // APIs but the canonical form is what other Windows tools display.
    VERIFY_ARE_EQUAL(std::wstring{ LR"(\\wsl$\Ubuntu\home\yeelam\.bashrc)" },
                     WslUncPath(L"Ubuntu", "/home/yeelam/.bashrc"));
    VERIFY_ARE_EQUAL(std::wstring{ LR"(\\wsl$\Debian-12\root\.intelligent-terminal)" },
                     WslUncPath(L"Debian-12", "/root/.intelligent-terminal"));
    VERIFY_ARE_EQUAL(std::wstring{ LR"(\\wsl$\Alpine\home\x\y\z)" },
                     WslUncPath(L"Alpine", "/home/x/y/z"));
    // Path without a leading slash gets the single separator inserted
    // (no double-backslash). Defensive — current callers always pass a
    // leading slash.
    VERIFY_ARE_EQUAL(std::wstring{ LR"(\\wsl$\Ubuntu\home\x)" },
                     WslUncPath(L"Ubuntu", "home/x"));
}

void ShellIntegrationTests::Wsl_StripExecTail_StripsExistingExecCommand()
{
    using Microsoft::Terminal::ShellIntegration::Wsl::details::StripExecTail;
    // No exec command -> returned unchanged.
    VERIFY_ARE_EQUAL(std::wstring_view{ L"wsl.exe -d Ubuntu" },
                     StripExecTail(L"wsl.exe -d Ubuntu", false));
    // --distribution-id GUID must NOT be mistaken for an exec terminator
    // (the ` -- ` needle is space-bounded; `--distribution-id` has no
    // trailing space after the dashes).
    VERIFY_ARE_EQUAL(std::wstring_view{ L"C:\\Windows\\system32\\wsl.exe --distribution-id {GUID}" },
                     StripExecTail(L"C:\\Windows\\system32\\wsl.exe --distribution-id {GUID}", false));
    // wsl.exe exec terminators (-e / --exec / --) are stripped so our probe
    // doesn't collide with a shell the profile already requested.
    VERIFY_ARE_EQUAL(std::wstring_view{ L"wsl.exe -d Ubuntu" },
                     StripExecTail(L"wsl.exe -d Ubuntu -e fish", false));
    VERIFY_ARE_EQUAL(std::wstring_view{ L"wsl.exe -d Ubuntu" },
                     StripExecTail(L"wsl.exe -d Ubuntu --exec zsh", false));
    VERIFY_ARE_EQUAL(std::wstring_view{ L"wsl.exe -d Ubuntu" },
                     StripExecTail(L"wsl.exe -d Ubuntu -- fish -l", false));
    // bash.exe: keep ONLY the launcher token; ALL its args are dropped (we
    // replace them with our own `-c "probe"`). bash treats a leading operand
    // like `~` as the script and would ignore a later `-c`, so the legacy
    // `bash.exe ~` launcher form must strip the operand too.
    VERIFY_ARE_EQUAL(std::wstring_view{ L"bash.exe" },
                     StripExecTail(L"bash.exe", true));
    VERIFY_ARE_EQUAL(std::wstring_view{ L"C:\\Windows\\System32\\bash.exe" },
                     StripExecTail(L"C:\\Windows\\System32\\bash.exe -c \"ls -la\"", true));
    VERIFY_ARE_EQUAL(std::wstring_view{ L"C:\\Windows\\System32\\bash.exe" },
                     StripExecTail(L"C:\\Windows\\System32\\bash.exe ~", true));
    VERIFY_ARE_EQUAL(std::wstring_view{ L"\"C:\\Windows\\System32\\bash.exe\"" },
                     StripExecTail(L"\"C:\\Windows\\System32\\bash.exe\" ~ -l", true));
    // Whitespace-robust: tabs / multiple spaces around the terminator, and a
    // `--` at end-of-string, are all handled (hand-edited commandlines).
    VERIFY_ARE_EQUAL(std::wstring_view{ L"wsl.exe  -d  Ubuntu" },
                     StripExecTail(L"wsl.exe  -d  Ubuntu   -e bash", false));
    VERIFY_ARE_EQUAL(std::wstring_view{ L"wsl.exe\t-d\tUbuntu" },
                     StripExecTail(L"wsl.exe\t-d\tUbuntu\t-e bash", false));
    VERIFY_ARE_EQUAL(std::wstring_view{ L"wsl.exe -d Ubuntu" },
                     StripExecTail(L"wsl.exe -d Ubuntu --", false));
    // A token that merely STARTS with a terminator string is NOT a terminator
    // (only whole-token matches cut) — already covered by the
    // `--distribution-id` case above, which starts with the `--` terminator
    // but must not be stripped.
}

void ShellIntegrationTests::Wsl_QualifyBareLauncher_QualifiesBareWslBash()
{
    using Microsoft::Terminal::ShellIntegration::Wsl::details::QualifyBareLauncher;
    // Build the expected prefix from the same OS-reported Windows dir the code
    // uses, so this is machine-independent.
    const std::wstring sys =
        std::wstring{ Microsoft::Terminal::ShellIntegration::details::WindowsDir() } + L"\\System32\\";
    // Bare wsl/bash launch tokens are qualified to the OS copy (option tail
    // preserved); a bare `wsl` gets the `.exe` too.
    VERIFY_ARE_EQUAL(sys + L"wsl.exe -d Ubuntu", QualifyBareLauncher(L"wsl.exe -d Ubuntu"));
    VERIFY_ARE_EQUAL(sys + L"wsl.exe", QualifyBareLauncher(L"wsl"));
    VERIFY_ARE_EQUAL(sys + L"bash.exe", QualifyBareLauncher(L"bash.exe"));
    // Already path-qualified, quoted, or a non-wsl/bash leaf -> unchanged.
    VERIFY_ARE_EQUAL(std::wstring{ L"C:\\Windows\\System32\\wsl.exe -d Ubuntu" },
                     QualifyBareLauncher(L"C:\\Windows\\System32\\wsl.exe -d Ubuntu"));
    VERIFY_ARE_EQUAL(std::wstring{ L"\"C:\\X\\wsl.exe\" -d Ubuntu" },
                     QualifyBareLauncher(L"\"C:\\X\\wsl.exe\" -d Ubuntu"));
    VERIFY_ARE_EQUAL(std::wstring{ L"cmd.exe /c wsl" }, QualifyBareLauncher(L"cmd.exe /c wsl"));
}

// ───────────────────────────────────────────────────────────────────
// Profile-presence gate (ShellIntegrationProfileGate.h)
//
// Verifies that we only install shell integration for shells the
// user has at least one profile for. A user keeping ONLY a
// "Developer PowerShell for VS" profile (which uses Windows
// PowerShell) and no default pwsh / Windows-PowerShell profile must
// still trigger Windows PowerShell install — and must NOT trigger
// pwsh install. WSL is not part of this gate (caller already
// iterates the WSL distro snapshot).
// ───────────────────────────────────────────────────────────────────

// Minimal profile double for AnyProfileUsesShell. The template only
// requires .Source() and .Commandline(); we expose them as wstring.
// AnyProfileUsesShell does `const auto src = profile.Source();` which
// MOVES the rvalue into a named local, extending its lifetime through
// the wstring_view{ src } construction below. The wstring_view itself
// does NOT extend the temporary's lifetime — that's a common
// misconception. The lifetime extension lives in the production
// helper, not in wstring_view's constructor.
namespace
{
    struct MockProfile
    {
        std::wstring src;
        std::wstring cmd;
        std::wstring Source() const { return src; }
        std::wstring Commandline() const { return cmd; }
    };
}

void ShellIntegrationTests::ProfileGate_PwshSourceMatches()
{
    // The dynamic generator's source is the strongest signal.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh,
                                        L"Windows.Terminal.PowershellCore",
                                        L"C:\\Program Files\\PowerShell\\7\\pwsh.exe"));
    // Source alone is enough — even with an unrelated commandline.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh,
                                        L"Windows.Terminal.PowershellCore",
                                        L""));
}

void ShellIntegrationTests::ProfileGate_PwshCommandlineLeafExeMatches()
{
    // WT-emitted quoted full path (the realistic form — paths with
    // spaces MUST be quoted in Windows commandlines).
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh,
                                        L"",
                                        L"\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -NoLogo"));
    // Bare-leaf form: user with pwsh on PATH may write `pwsh -arg`.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh,
                                        L"",
                                        L"pwsh -WorkingDirectory ~"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh, L"", L"pwsh.exe"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh, L"", L"pwsh"));
    // Case-insensitive.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh, L"", L"PWSH.EXE"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh, L"", L"Pwsh"));
    // Launch exe is cmd.exe; `pwsh` is just an arg — MUST NOT match.
    // This is the difference between any-token matching and launch-exe
    // matching: the user is running cmd, not pwsh.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Pwsh,
                                         L"",
                                         L"cmd.exe /c echo pwsh"));
    // Names whose leaf contains "pwsh" as a substring but doesn't
    // equal "pwsh" or "pwsh.exe" must NOT match (the matcher anchors
    // on the full leaf token, not any substring).
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Pwsh, L"", L"pwshell.exe"));
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Pwsh, L"", L"pwsh-preview.exe"));
    // Unquoted path containing spaces (e.g. the default Git/PowerShell
    // install under "C:\Program Files\…"). CreateProcessW still launches
    // these by probing progressively longer space-split prefixes, so the
    // profile DOES run — and we must recognize it. The matcher extends the
    // launch-exe token across spaces to the first ".exe" boundary when the
    // commandline begins at a filesystem root.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh,
                                        L"",
                                        L"C:\\Program Files\\PowerShell\\7\\pwsh.exe"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh,
                                        L"",
                                        L"C:\\Program Files\\PowerShell\\7\\pwsh.exe -NoLogo"));
    // Extensionless rooted pwsh path with spaces — resolved by .exe probing.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh,
                                        L"",
                                        L"C:\\Program Files\\PowerShell\\7\\pwsh -NoLogo"));
    // The "PowerShell" directory component must NOT be mistaken for a
    // `powershell` launch leaf (the leaf scan requires a trailing
    // whitespace/end after the segment, which "\PowerShell\" lacks).
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::WindowsPowerShell,
                                         L"Windows.Terminal.PowershellCore",
                                         L"C:\\Program Files\\PowerShell\\7\\pwsh -NoLogo"));
    // Bare command whose launch exe is NOT a path-root MUST NOT absorb a
    // later `.exe` arg: launch is cmd, not pwsh.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Pwsh,
                                         L"",
                                         L"cmd /c C:\\Program Files\\PowerShell\\7\\pwsh.exe"));
    // Degenerate inputs: empty commandline, and a non-empty commandline
    // matched against an empty leaf, both return false without crashing.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Pwsh, L"", L""));
    // Leading whitespace before a rooted unquoted path is tolerated.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Pwsh,
                                        L"",
                                        L"   C:\\Program Files\\PowerShell\\7\\pwsh.exe"));
}

void ShellIntegrationTests::ProfileGate_WindowsPowerShellOnlyWhenNotPwsh()
{
    // The classic Windows PowerShell profile.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::WindowsPowerShell,
                                        L"",
                                        L"powershell.exe"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::WindowsPowerShell,
                                        L"",
                                        L"C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"));
    // pwsh.exe lives under a folder containing "PowerShell" — naive
    // substring on "powershell" would mis-match. The anchor on the
    // leaf "powershell.exe" (no "pwsh.exe") prevents this.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::WindowsPowerShell,
                                         L"Windows.Terminal.PowershellCore",
                                         L"C:\\Program Files\\PowerShell\\7\\pwsh.exe"));
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::WindowsPowerShell,
                                         L"",
                                         L"pwsh.exe"));
}

void ShellIntegrationTests::ProfileGate_WindowsPowerShellWithDeveloperVsProfile()
{
    // "Developer PowerShell for VS 2022" — uses Windows PowerShell
    // under the hood, even though the profile name is custom. This
    // is the exact scenario the user called out: a user may have
    // deleted the default Windows PowerShell profile but kept the
    // VS Developer one, and we must still install for that shell.
    const auto cmd = LR"(C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe -NoExit -Command "&{Import-Module 'C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\Microsoft.VisualStudio.DevShell.dll'; Enter-VsDevShell ...}")";
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::WindowsPowerShell, L"", cmd));
    // And does NOT trigger pwsh.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Pwsh, L"", cmd));
}

void ShellIntegrationTests::ProfileGate_BashOnlyForGitBashNotWsl()
{
    // Git Bash — quoted full path (the realistic form).
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash,
                                        L"",
                                        L"\"C:\\Program Files\\Git\\bin\\bash.exe\" -i -l"));
    // Git Bash — UNQUOTED full path with spaces. This is the default
    // Git-for-Windows install location ("C:\Program Files\Git") and the
    // exact form a user gets when they type/paste the path into the
    // profile commandline without quotes. CreateProcessW launches it by
    // probing space-split prefixes, so it runs — and integration MUST be
    // recognized. Regression guard for the silent "leaf=Program" skip.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash,
                                        L"",
                                        L"C:\\Program Files\\Git\\bin\\bash.exe"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash,
                                        L"",
                                        L"C:\\Program Files\\Git\\bin\\bash.exe -i -l"));
    // Git Bash — UNQUOTED rooted path with spaces and NO `.exe` extension.
    // CreateProcessW resolves it by probing space-split prefixes and
    // appending `.exe`, so the profile launches bash and integration MUST
    // be recognized (regression guard for the reviewer-flagged gap).
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash,
                                        L"",
                                        L"C:\\Program Files\\Git\\bin\\bash -i -l"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash,
                                        L"",
                                        L"C:\\Program Files\\Git\\bin\\bash"));
    // Extensionless rooted bash path must NOT be mistaken for pwsh — the
    // leaf scan only matches `\<leaf>` segments terminated by whitespace/end.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Pwsh,
                                         L"",
                                         L"C:\\Program Files\\Git\\bin\\bash -i -l"));
    // UNC path root (\\server\share\...) — another filesystem root the
    // unquoted-with-spaces heuristic must accept, both with and without
    // the `.exe` extension.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash,
                                        L"",
                                        L"\\\\server\\share\\Git\\bin\\bash.exe -i"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash,
                                        L"",
                                        L"\\\\server\\share\\Git\\bin\\bash -i"));
    // Forward-slash separators with a drive root (C:/...). Both the root
    // detection and the leaf/`.exe` boundary scans must treat `/` like `\`.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash,
                                        L"",
                                        L"C:/Program Files/Git/bin/bash.exe"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash,
                                        L"",
                                        L"C:/Program Files/Git/bin/bash -i -l"));
    // Bare leaf — user with bash on PATH.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash, L"", L"bash"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash, L"", L"bash.exe -i"));
    // WSL distro profile — uses wsl.exe. Must NOT match the Git Bash
    // target (it would be a duplicate install since WSL distros are
    // handled separately by per-distro iteration).
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash,
                                         L"Windows.Terminal.Wsl",
                                         L"wsl.exe -d Ubuntu"));
    // Bare wsl too.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash, L"", L"wsl -d Ubuntu"));
    // Launch is wsl.exe; bash.exe later in args MUST NOT match Bash
    // (we anchor on the launch exe, not any token).
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash,
                                         L"",
                                         L"wsl.exe -d Ubuntu -e bash.exe -l"));
    // Unquoted path-root launch is wsl.exe; the first ".exe" boundary wins
    // so a trailing bash.exe arg still does NOT match Bash.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash,
                                         L"",
                                         L"C:\\Windows\\System32\\wsl.exe -d Ubuntu -e bash.exe"));
    // Legacy System32 bash.exe is the WSL default-distro launcher, NOT
    // Git Bash — it must not be classified as Git Bash (it runs inside
    // WSL whose $HOME is not %USERPROFILE%, so a Windows .bashrc would
    // be a silent no-op).
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash,
                                         L"",
                                         L"C:\\Windows\\System32\\bash.exe ~"));
}

void ShellIntegrationTests::ProfileGate_BashRejectsSystem32WslLauncher()
{
    // System32 / Sysnative bash.exe → WSL default distro, not Git Bash.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash, L"", L"C:\\Windows\\System32\\bash.exe"));
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash, L"", L"\"C:\\Windows\\System32\\bash.exe\" ~"));
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash, L"", L"C:\\Windows\\Sysnative\\bash.exe"));
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash, L"", L"c:\\windows\\system32\\BASH.EXE -l"));
    // Forward-slash separators must be treated like backslashes.
    VERIFY_IS_FALSE(ProfileMatchesShell(Target::Bash, L"", L"C:/Windows/System32/bash.exe ~"));
    // Real Git Bash (under Program Files) and bare leaf still match.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash, L"", L"\"C:\\Program Files\\Git\\bin\\bash.exe\" -i -l"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash, L"", L"bash.exe -i"));
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash, L"", L"bash"));
    // A bash.exe NOT under the real Windows System32/Sysnative directory
    // (resolved via GetWindowsDirectoryW) is treated as Git Bash — the check
    // anchors the launch-token prefix to the actual %SystemRoot%, so an
    // unrelated path like C:\tools\bash.exe is not excluded.
    VERIFY_IS_TRUE(ProfileMatchesShell(Target::Bash, L"", L"C:\\tools\\bash.exe"));
}

void ShellIntegrationTests::IsWslProfile_WslLauncherForms_True()
{
    // Every wsl.exe launch form is a WSL profile, recognized purely from the
    // commandline (no Source) — the installer reuses the commandline and
    // probes the distro, so we never parse `-d` / `--distribution-id` /
    // Name(). This covers the common sourceless custom-profile gap, the
    // legacy generator form, AND the modern Store `--distribution-id` form.
    VERIFY_IS_TRUE(IsWslProfile(L"wsl.exe -d Ubuntu"));
    VERIFY_IS_TRUE(IsWslProfile(L"wsl -d Ubuntu"));
    VERIFY_IS_TRUE(IsWslProfile(L"wsl.exe ~ -d Ubuntu"));
    VERIFY_IS_TRUE(IsWslProfile(L"wsl.exe --distribution Debian"));
    VERIFY_IS_TRUE(IsWslProfile(L"C:\\Windows\\System32\\wsl.exe -d Ubuntu-22.04"));
    VERIFY_IS_TRUE(IsWslProfile(L"C:\\Windows\\system32\\wsl.exe --distribution-id {6f8e9a45-40ca-470d-a649-30afc57d2a57}"));
    // Bare wsl.exe (default distro) is supported — the probe resolves the
    // default distro at runtime, so we no longer have to skip it.
    VERIFY_IS_TRUE(IsWslProfile(L"wsl.exe"));
    VERIFY_IS_TRUE(IsWslProfile(L"wsl"));
}

void ShellIntegrationTests::IsWslProfile_System32BashLauncher_True()
{
    // Legacy System32 / Sysnative bash.exe IS the WSL default-distro
    // launcher (runs bash in the default distro), so it's a WSL profile —
    // even though ProfileMatchesShell(Bash) deliberately excludes it from
    // Git Bash.
    VERIFY_IS_TRUE(IsWslProfile(L"C:\\Windows\\System32\\bash.exe"));
    VERIFY_IS_TRUE(IsWslProfile(L"C:\\Windows\\System32\\bash.exe ~"));
    VERIFY_IS_TRUE(IsWslProfile(L"C:\\Windows\\Sysnative\\bash.exe"));
    VERIFY_IS_TRUE(IsWslProfile(L"C:/Windows/System32/bash.exe"));
}

void ShellIntegrationTests::IsWslProfile_GitBashAndOthers_False()
{
    // Git Bash (under Program Files) is NOT WSL.
    VERIFY_IS_FALSE(IsWslProfile(L"\"C:\\Program Files\\Git\\bin\\bash.exe\" -i -l"));
    VERIFY_IS_FALSE(IsWslProfile(L"C:\\Program Files\\Git\\bin\\bash.exe -i -l"));
    VERIFY_IS_FALSE(IsWslProfile(L"bash.exe -i"));
    VERIFY_IS_FALSE(IsWslProfile(L"pwsh.exe"));
    // A longer leaf under System32 starting with "bash" must NOT match the
    // System32-bash launcher (leaf-boundary check after `…\bash`).
    VERIFY_IS_FALSE(IsWslProfile(L"C:\\Windows\\System32\\bashful.exe"));
    // Anchored on the launch exe: `cmd /c wsl …` launches cmd, not wsl.
    VERIFY_IS_FALSE(IsWslProfile(L"cmd.exe /c wsl -d Ubuntu"));
}

void ShellIntegrationTests::ProfileGate_AnyProfileEmptyCollection()
{
    std::vector<MockProfile> profiles;
    VERIFY_IS_FALSE(AnyProfileUsesShell(Target::Pwsh, profiles));
    VERIFY_IS_FALSE(AnyProfileUsesShell(Target::WindowsPowerShell, profiles));
    VERIFY_IS_FALSE(AnyProfileUsesShell(Target::Bash, profiles));
}

void ShellIntegrationTests::ProfileGate_AnyProfileFindsOne()
{
    std::vector<MockProfile> profiles = {
        { L"", L"cmd.exe" },
        { L"", LR"(C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe)" },
        // Git Bash — quoted (realistic Windows commandline form for
        // paths with spaces). Bare `bash.exe` also fine.
        { L"", LR"("C:\Program Files\Git\bin\bash.exe" -i -l)" },
    };
    // Pwsh missing, Windows PowerShell + Bash present.
    VERIFY_IS_FALSE(AnyProfileUsesShell(Target::Pwsh, profiles));
    VERIFY_IS_TRUE(AnyProfileUsesShell(Target::WindowsPowerShell, profiles));
    VERIFY_IS_TRUE(AnyProfileUsesShell(Target::Bash, profiles));
}

void ShellIntegrationTests::ProfileGate_AnyProfileMissingShellReturnsFalse()
{
    // The "Developer PowerShell for VS" only scenario: user deleted
    // the default Windows PowerShell profile AND has no pwsh / bash.
    // Only the VS Developer profile remains.
    std::vector<MockProfile> profiles = {
        { L"", LR"(C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe -NoExit -Command "&{Import-Module ...}")" },
    };
    VERIFY_IS_TRUE(AnyProfileUsesShell(Target::WindowsPowerShell, profiles));
    VERIFY_IS_FALSE(AnyProfileUsesShell(Target::Pwsh, profiles));
    VERIFY_IS_FALSE(AnyProfileUsesShell(Target::Bash, profiles));
}