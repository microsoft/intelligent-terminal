// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/WslProfileHealthResolver.h"

using namespace Microsoft::Terminal::ShellIntegration::Health;
using namespace Microsoft::Terminal::ShellIntegration::Wsl::ProfileHealth;
using namespace WEX::TestExecution;
namespace Invocation = Microsoft::Terminal::ShellIntegration::Invocation;

namespace TerminalAppUnitTests
{
    class WslProfileHealthResolverTests
    {
        TEST_CLASS(WslProfileHealthResolverTests);

        TEST_METHOD(BuildsUbuntuBashTarget);
        TEST_METHOD(BuildsTargetForNestedHome);
        TEST_METHOD(UsesDistroIdentityToSeparateTargets);
        TEST_METHOD(RejectsUnsafeIdentity);
        TEST_METHOD(RejectsDefaultWslWithoutProbing);
    };

    void WslProfileHealthResolverTests::BuildsUbuntuBashTarget()
    {
        const auto target = TryCreateTargetFromVerifiedIdentity(L"Ubuntu", "/home/alice");

        VERIFY_IS_TRUE(target.has_value());
        VERIFY_ARE_EQUAL(ShellKind::WslBash, target->shell);
        VERIFY_ARE_EQUAL(ProfileSyntax::Bash, target->syntax);
        VERIFY_ARE_EQUAL(std::wstring{ L"\\\\wsl$\\Ubuntu\\home\\alice\\.bashrc" }, target->profilePath);
        VERIFY_IS_TRUE(target->hostPath.empty());
        VERIFY_ARE_EQUAL(std::wstring{ L"wsl:Ubuntu" }, target->shellIdentity);
    }

    void WslProfileHealthResolverTests::BuildsTargetForNestedHome()
    {
        const auto target = TryCreateTargetFromVerifiedIdentity(L"Ubuntu-24.04", "/srv/users/alice-dev");

        VERIFY_IS_TRUE(target.has_value());
        VERIFY_ARE_EQUAL(
            std::wstring{ L"\\\\wsl$\\Ubuntu-24.04\\srv\\users\\alice-dev\\.bashrc" },
            target->profilePath);
    }

    void WslProfileHealthResolverTests::UsesDistroIdentityToSeparateTargets()
    {
        const auto ubuntu = TryCreateTargetFromVerifiedIdentity(L"Ubuntu", "/home/alice");
        const auto debian = TryCreateTargetFromVerifiedIdentity(L"Debian", "/home/alice");

        VERIFY_IS_TRUE(ubuntu.has_value());
        VERIFY_IS_TRUE(debian.has_value());
        VERIFY_IS_FALSE(*ubuntu == *debian);
        VERIFY_ARE_NOT_EQUAL(ubuntu->shellIdentity, debian->shellIdentity);
        VERIFY_ARE_NOT_EQUAL(ubuntu->profilePath, debian->profilePath);
    }

    void WslProfileHealthResolverTests::RejectsUnsafeIdentity()
    {
        VERIFY_IS_FALSE(TryCreateTargetFromVerifiedIdentity(L"Ubuntu;whoami", "/home/alice").has_value());
        VERIFY_IS_FALSE(TryCreateTargetFromVerifiedIdentity(L"Ubuntu", "/home/alice/../root").has_value());
        VERIFY_IS_FALSE(TryCreateTargetFromVerifiedIdentity(L"Ubuntu", "home/alice").has_value());
    }

    void WslProfileHealthResolverTests::RejectsDefaultWslWithoutProbing()
    {
        Invocation::Classification classification;
        classification.shellKind = ShellKind::WslBash;
        classification.wsl.launcher = Invocation::WslLauncher::WslExe;
        classification.wsl.guestShell = Invocation::WslGuestShell::Default;
        classification.wsl.requiresDefaultShellConfirmation = true;

        const auto resolution = ResolveTarget(L"C:\\Windows\\System32\\wsl.exe", classification);

        VERIFY_IS_FALSE(resolution.target.has_value());
        VERIFY_ARE_EQUAL(ResolutionFailure::NotExplicitBash, resolution.failure);
    }
}
