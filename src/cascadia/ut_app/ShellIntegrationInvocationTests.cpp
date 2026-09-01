// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/ShellIntegrationInvocation.h"

using namespace Microsoft::Terminal::ShellIntegration::Invocation;
using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    namespace
    {
        std::wstring WindowsDirectory()
        {
            wchar_t path[MAX_PATH]{};
            const auto length = GetWindowsDirectoryW(path, static_cast<UINT>(std::size(path)));
            VERIFY_IS_TRUE(length > 0 && length < std::size(path));
            auto trimmedLength = length;
            while (trimmedLength > 0 && (path[trimmedLength - 1] == L'\\' || path[trimmedLength - 1] == L'/'))
            {
                --trimmedLength;
            }
            return { path, trimmedLength };
        }

        std::wstring Commandline(const std::wstring_view root, const std::wstring_view arguments = {})
        {
            return L"\"" + std::wstring{ root } + L"\"" + std::wstring{ arguments };
        }
    }

    class ShellIntegrationInvocationTests
    {
        TEST_CLASS(ShellIntegrationInvocationTests);

        TEST_METHOD(ClassifiesPowerShellHostSwitches);
        TEST_METHOD(ClassifiesNoExitPowerShellCommandSessions);
        TEST_METHOD(DoesNotParsePowerShellCommandTextAsHostOptions);
        TEST_METHOD(RejectsWrappersRelativeRootsAndMismatchedImages);
        TEST_METHOD(ClassifiesGitBashOnlyWhenBashrcIsProven);
        TEST_METHOD(ParsesWslSelectionAndGuestBoundaries);
        TEST_METHOD(RejectsUnsupportedWslGuestsAndDefaults);
    };

    void ShellIntegrationInvocationTests::ClassifiesPowerShellHostSwitches()
    {
        const auto eligible = Classify(L"\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -NoL -NoE");
        VERIFY_IS_TRUE(eligible.eligible);
        VERIFY_ARE_EQUAL(Reason::Eligible, eligible.reason);
        VERIFY_IS_TRUE(Classify(L"\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -Login -NoLogo").eligible);

        const auto windowsPowerShell = WindowsDirectory() + L"\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
        VERIFY_ARE_EQUAL(Reason::PowerShellNoProfile,
                         Classify(Commandline(windowsPowerShell, L" -NoP")).reason);
        VERIFY_ARE_EQUAL(Reason::PowerShellNonInteractive,
                         Classify(Commandline(windowsPowerShell, L" -non")).reason);
        VERIFY_ARE_EQUAL(Reason::PowerShellFile,
                         Classify(Commandline(windowsPowerShell, L" -f setup.ps1")).reason);
        VERIFY_ARE_EQUAL(Reason::PowerShellEncodedCommand,
                         Classify(Commandline(windowsPowerShell, L" -e ZQBjAGgAbwA=")).reason);
        VERIFY_IS_TRUE(Classify(Commandline(WindowsDirectory() + L"\\SysWOW64\\WindowsPowerShell\\v1.0\\powershell.exe", L" -NoLogo")).eligible);
        VERIFY_ARE_EQUAL(Reason::UnsupportedRoot, Classify(L"D:\\Tools\\powershell.exe -NoLogo").reason);
        VERIFY_ARE_EQUAL(Reason::UnsupportedRoot,
                         Classify(L"powershell.exe -NoLogo", std::wstring_view{ L"D:\\Tools\\powershell.exe" }).reason);
    }

    void ShellIntegrationInvocationTests::ClassifiesNoExitPowerShellCommandSessions()
    {
        constexpr std::wstring_view developerShellCommand{
            LR"( -NoExit -Command "&{Import-Module 'C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\Microsoft.VisualStudio.DevShell.dll'; Enter-VsDevShell 1234}")"
        };

        constexpr std::wstring_view pwshImage{ L"C:\\Program Files\\PowerShell\\7\\pwsh.exe" };
        VERIFY_IS_TRUE(Classify(std::wstring{ L"pwsh.exe" } + std::wstring{ developerShellCommand }, pwshImage).eligible);

        const auto windowsPowerShell = WindowsDirectory() + L"\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
        const auto windowsDeveloperShell = Classify(std::wstring{ L"powershell.exe" } + std::wstring{ developerShellCommand }, windowsPowerShell);
        VERIFY_IS_TRUE(windowsDeveloperShell.eligible);
        VERIFY_ARE_EQUAL(Reason::Eligible, windowsDeveloperShell.reason);

        VERIFY_ARE_EQUAL(Reason::PowerShellNoProfile,
                         Classify(Commandline(windowsPowerShell, L" -NoProfile -NoExit -Command \"echo ready\"")).reason);
        VERIFY_ARE_EQUAL(Reason::PowerShellNonInteractive,
                         Classify(Commandline(windowsPowerShell, L" -NonInteractive -NoExit -Command \"echo ready\"")).reason);
        VERIFY_ARE_EQUAL(Reason::PowerShellCommand,
                         Classify(Commandline(windowsPowerShell, L" -Command \"echo ready\"")).reason);
        VERIFY_ARE_EQUAL(Reason::PowerShellEncodedCommand,
                         Classify(Commandline(windowsPowerShell, L" -NoExit -EncodedCommand ZQBjAGgAbwA=")).reason);
        VERIFY_ARE_EQUAL(Reason::PowerShellFile,
                         Classify(Commandline(windowsPowerShell, L" -NoExit -File setup.ps1")).reason);
    }

    void ShellIntegrationInvocationTests::DoesNotParsePowerShellCommandTextAsHostOptions()
    {
        const auto classified = Classify(L"\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -Command \"echo -NoProfile\"");
        VERIFY_ARE_EQUAL(Reason::PowerShellCommand, classified.reason);

        const auto persistent = Classify(L"\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -NoExit -Command \"echo -NoProfile\"");
        VERIFY_IS_TRUE(persistent.eligible);
        VERIFY_ARE_EQUAL(Reason::Eligible, persistent.reason);
    }

    void ShellIntegrationInvocationTests::RejectsWrappersRelativeRootsAndMismatchedImages()
    {
        const auto system32 = WindowsDirectory() + L"\\System32";
        VERIFY_ARE_EQUAL(Reason::UnsupportedRoot,
                         Classify(Commandline(system32 + L"\\cmd.exe", L" /c pwsh.exe -NoProfile")).reason);
        VERIFY_ARE_EQUAL(Reason::RootNotAbsolute, Classify(L"pwsh.exe -NoLogo").reason);
        VERIFY_ARE_EQUAL(Reason::ActualRootLeafMismatch,
                         Classify(L"\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\"",
                                  std::wstring_view{ L"C:\\Windows\\System32\\cmd.exe" }).reason);
        VERIFY_ARE_EQUAL(Reason::ActualRootPathMismatch,
                         Classify(L"\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\"",
                                  std::wstring_view{ L"D:\\Tools\\pwsh.exe" }).reason);
        VERIFY_ARE_EQUAL(Reason::RootNotAbsolute,
                         Classify(L".\\pwsh.exe",
                                  std::wstring_view{ L"C:\\Program Files\\PowerShell\\7\\pwsh.exe" }).reason);
        VERIFY_IS_TRUE(Classify(L"pwsh.exe -NoLogo",
                                std::wstring_view{ L"C:\\Program Files\\PowerShell\\7\\pwsh.exe" }).eligible);
        VERIFY_ARE_EQUAL(Reason::WslMalformedHostOption,
                         Classify(Commandline(system32 + L"\\wsl.exe", L" -d Ubuntu --distribution Debian -e bash -i")).reason);
        VERIFY_ARE_EQUAL(Reason::UnsupportedRoot,
                         Classify(L"C:\\malware\\System32\\wsl.exe -d Ubuntu -e bash -i").reason);
        VERIFY_ARE_EQUAL(Reason::UnsupportedRoot,
                         Classify(L"wsl.exe -d Ubuntu -e bash -i", std::wstring_view{ L"C:\\malware\\System32\\wsl.exe" }).reason);
    }

    void ShellIntegrationInvocationTests::ClassifiesGitBashOnlyWhenBashrcIsProven()
    {
        constexpr std::wstring_view bash{ L"\"C:\\Program Files\\Git\\bin\\bash.exe\"" };
        VERIFY_IS_TRUE(Classify(std::wstring{ bash } + L" -i").eligible);
        VERIFY_IS_TRUE(Classify(std::wstring{ bash } + L" -il").eligible);
        VERIFY_IS_TRUE(Classify(std::wstring{ bash } + L" --login -i").eligible);
        VERIFY_IS_TRUE(Classify(bash).eligible);
        VERIFY_IS_TRUE(Classify(L"C:\\PortableGit\\bin\\bash.exe -i").eligible);
        VERIFY_ARE_EQUAL(Reason::BashNoRc, Classify(std::wstring{ bash } + L" -i --norc").reason);
        VERIFY_ARE_EQUAL(Reason::BashCustomRcFile, Classify(std::wstring{ bash } + L" -i --rcfile customrc").reason);
        VERIFY_ARE_EQUAL(Reason::BashCommand, Classify(std::wstring{ bash } + L" -ic \"echo hi\"").reason);
        VERIFY_ARE_EQUAL(Reason::BashScriptOperand, Classify(std::wstring{ bash } + L" -i -- script.sh").reason);
        VERIFY_ARE_EQUAL(Reason::BashLoginNoProfile, Classify(std::wstring{ bash } + L" --login --noprofile -i").reason);
        VERIFY_ARE_EQUAL(Reason::BashPosixMode, Classify(std::wstring{ bash } + L" -i --posix").reason);
        VERIFY_ARE_EQUAL(Reason::BashPosixMode, Classify(std::wstring{ bash } + L" -i -o posix").reason);
    }

    void ShellIntegrationInvocationTests::ParsesWslSelectionAndGuestBoundaries()
    {
        const auto windowsDirectory = WindowsDirectory();
        const auto wsl = windowsDirectory + L"\\System32\\wsl.exe";
        const auto classified = Classify(Commandline(wsl, L" --distribution Ubuntu --exec bash -i"));
        VERIFY_IS_TRUE(classified.eligible);
        VERIFY_IS_TRUE(classified.wsl.distribution.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"Ubuntu" }, *classified.wsl.distribution);
        VERIFY_ARE_EQUAL(WslGuestShell::Bash, classified.wsl.guestShell);
        VERIFY_IS_TRUE(classified.wsl.usesExec);

        const auto commandBoundary = Classify(Commandline(wsl, L" -d Ubuntu -- bash -ic \"echo --norc\""));
        VERIFY_ARE_EQUAL(Reason::BashCommand, commandBoundary.reason);

        const auto distributionId = Classify(Commandline(wsl, L" --distribution-id {1234} -e bash --norc"));
        VERIFY_ARE_EQUAL(Reason::BashNoRc, distributionId.reason);
        VERIFY_IS_TRUE(distributionId.wsl.distributionId.has_value());
        VERIFY_ARE_EQUAL(
            Reason::BashLoginProfileUnproven,
            Classify(Commandline(wsl, L" -d Ubuntu -e bash -l")).reason);

        const auto sysnative = windowsDirectory + L"\\Sysnative\\wsl.exe";
        VERIFY_IS_TRUE(Classify(Commandline(sysnative, L" -d Ubuntu -e bash -i"), wsl).eligible);
    }

    void ShellIntegrationInvocationTests::RejectsUnsupportedWslGuestsAndDefaults()
    {
        const auto system32 = WindowsDirectory() + L"\\System32";
        VERIFY_ARE_EQUAL(Reason::WslExplicitFish,
                         Classify(Commandline(system32 + L"\\wsl.exe", L" -d Ubuntu -e fish")).reason);
        VERIFY_ARE_EQUAL(Reason::WslExplicitZsh,
                         Classify(Commandline(system32 + L"\\wsl.exe", L" -d Ubuntu -e zsh")).reason);
        VERIFY_ARE_EQUAL(Reason::WslMissingGuestCommand,
                         Classify(Commandline(system32 + L"\\wsl.exe", L" -d Ubuntu -e")).reason);

        const auto bare = Classify(Commandline(system32 + L"\\wsl.exe"));
        VERIFY_ARE_EQUAL(Reason::WslIdentityRequired, bare.reason);
        VERIFY_IS_TRUE(bare.wsl.requiresIdentity);
        VERIFY_IS_TRUE(bare.wsl.requiresDefaultShellConfirmation);

        const auto legacy = Classify(Commandline(system32 + L"\\bash.exe", L" -i"));
        VERIFY_ARE_EQUAL(Reason::WslIdentityRequired, legacy.reason);
        VERIFY_ARE_EQUAL(WslLauncher::LegacySystem32Bash, legacy.wsl.launcher);

        const auto windowsDirectory = WindowsDirectory();
        VERIFY_ARE_EQUAL(Reason::WslIdentityRequired,
                         Classify(Commandline(windowsDirectory + L"\\Sysnative\\bash.exe", L" -i"),
                                  windowsDirectory + L"\\System32\\bash.exe").reason);
    }
}
