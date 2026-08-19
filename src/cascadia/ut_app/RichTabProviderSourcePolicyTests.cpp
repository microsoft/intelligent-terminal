// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../RichTabProvider/ProviderSourcePolicy.h"

using namespace Microsoft::Terminal::RichTab::Provider;

namespace TerminalAppUnitTests
{
    class RichTabProviderSourcePolicyTests
    {
        TEST_CLASS(RichTabProviderSourcePolicyTests);

        TEST_METHOD(DefinesReleaseTrustAndPrecedence);
        TEST_METHOD(AppExtensionConsentSurvivesPackageUpdate);
        TEST_METHOD(DevelopmentConsentBindsProviderAndRoot);
    };

    void RichTabProviderSourcePolicyTests::DefinesReleaseTrustAndPrecedence()
    {
        VERIFY_IS_TRUE(
            ProviderSourcePrecedence(ProviderSourceKind::BuiltIn) >
            ProviderSourcePrecedence(ProviderSourceKind::AppExtension));
        VERIFY_IS_TRUE(
            ProviderSourcePrecedence(ProviderSourceKind::AppExtension) >
            ProviderSourcePrecedence(ProviderSourceKind::Development));
        VERIFY_IS_TRUE(
            ProviderSourcePrecedence(ProviderSourceKind::Development) >
            ProviderSourcePrecedence(ProviderSourceKind::LegacyManaged));

        VERIFY_ARE_EQUAL(
            ProviderConsentRequirement::NotRequired,
            ConsentRequirementFor(ProviderSourceKind::BuiltIn));
        VERIFY_ARE_EQUAL(
            ProviderConsentRequirement::Required,
            ConsentRequirementFor(ProviderSourceKind::AppExtension));
        VERIFY_ARE_EQUAL(
            ProviderConsentRequirement::Required,
            ConsentRequirementFor(ProviderSourceKind::Development));
        VERIFY_ARE_EQUAL(
            ProviderConsentRequirement::SourceNotAllowed,
            ConsentRequirementFor(ProviderSourceKind::LegacyManaged));
        VERIFY_IS_TRUE(IsReservedBuiltInProviderId(
            "com.microsoft.intelligent-terminal.git-status"));
    }

    void RichTabProviderSourcePolicyTests::AppExtensionConsentSurvivesPackageUpdate()
    {
        const auto first = AppExtensionSourceIdentity(
            "io.example.provider",
            L"rich-tabs",
            L"Example.Provider_123",
            L"Example.Provider_1.0.0.0_x64__123",
            L"publisher123",
            L"1.0.0.0");
        const auto updated = AppExtensionSourceIdentity(
            "io.example.provider",
            L"rich-tabs",
            L"Example.Provider_123",
            L"Example.Provider_2.0.0.0_x64__123",
            L"publisher123",
            L"2.0.0.0");
        const auto differentExtension = AppExtensionSourceIdentity(
            "io.example.provider",
            L"other-extension",
            L"Example.Provider_123",
            L"Example.Provider_2.0.0.0_x64__123",
            L"publisher123",
            L"2.0.0.0");

        VERIFY_IS_TRUE(ProviderConsentKey(first).has_value());
        VERIFY_ARE_EQUAL(*ProviderConsentKey(first), *ProviderConsentKey(updated));
        VERIFY_ARE_NOT_EQUAL(
            *ProviderConsentKey(first),
            *ProviderConsentKey(differentExtension));
    }

    void RichTabProviderSourcePolicyTests::DevelopmentConsentBindsProviderAndRoot()
    {
        const auto testRoot =
            std::filesystem::temp_directory_path() /
            (L"rich-tab-source-policy-test-" +
             std::to_wstring(GetCurrentProcessId()) +
             L"-" +
             std::to_wstring(GetTickCount64()));
        const auto firstRoot = testRoot / L"provider";
        const auto differentRoot = testRoot / L"other";
        std::filesystem::create_directories(firstRoot);
        std::filesystem::create_directories(differentRoot);
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(testRoot, error);
        });

        const auto first = DevelopmentSourceIdentity(
            "io.example.provider",
            firstRoot);
        const auto equivalent = DevelopmentSourceIdentity(
            "io.example.provider",
            firstRoot / L".");
        const auto differentSource = DevelopmentSourceIdentity(
            "io.example.provider",
            differentRoot);
        const auto differentProvider = DevelopmentSourceIdentity(
            "io.example.other",
            firstRoot);

        VERIFY_IS_TRUE(ProviderConsentKey(first).has_value());
        VERIFY_ARE_EQUAL(*ProviderConsentKey(first), *ProviderConsentKey(equivalent));
        VERIFY_ARE_NOT_EQUAL(*ProviderConsentKey(first), *ProviderConsentKey(differentSource));
        VERIFY_ARE_NOT_EQUAL(*ProviderConsentKey(first), *ProviderConsentKey(differentProvider));
    }
}
