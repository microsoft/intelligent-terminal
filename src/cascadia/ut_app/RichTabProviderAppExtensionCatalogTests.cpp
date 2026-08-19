// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../RichTabProvider/AppExtensionProviderCatalog.h"

#include <fstream>

using namespace Microsoft::Terminal::RichTab::Provider;

namespace TerminalAppUnitTests
{
    class RichTabProviderAppExtensionCatalogTests
    {
        TEST_CLASS(RichTabProviderAppExtensionCatalogTests);

        TEST_METHOD(DiscoversValidCandidateWithoutActivatingIt);
        TEST_METHOD(ReportsInvalidIdentityAndPackageStatus);
        TEST_METHOD(ReportsMissingAndInvalidManifest);
        TEST_METHOD(RejectsDuplicateProviderIds);

        struct TestDirectory
        {
            std::filesystem::path root;

            TestDirectory()
            {
                root = std::filesystem::temp_directory_path() /
                       (L"rich-tab-app-extension-test-" +
                        std::to_wstring(GetCurrentProcessId()) +
                        L"-" +
                        std::to_wstring(GetTickCount64()));
                std::filesystem::create_directories(root);
            }

            ~TestDirectory()
            {
                std::error_code error;
                std::filesystem::remove_all(root, error);
            }
        };

        static AppExtensionCandidate Candidate(
            const std::filesystem::path& path,
            std::wstring extensionId = L"git",
            bool packageHealthy = true);
        static void WriteManifest(
            const std::filesystem::path& path,
            std::string_view providerId);
    };

    AppExtensionCandidate RichTabProviderAppExtensionCatalogTests::Candidate(
        const std::filesystem::path& path,
        std::wstring extensionId,
        const bool packageHealthy)
    {
        return {
            AppExtensionSourceIdentity(
                {},
                std::move(extensionId),
                L"Tests.RichTabProvider_123",
                L"Tests.RichTabProvider_1.0.0.0_x64__123",
                L"publisher123",
                L"1.0.0.0"),
            path,
            packageHealthy,
        };
    }

    void RichTabProviderAppExtensionCatalogTests::WriteManifest(
        const std::filesystem::path& path,
        const std::string_view providerId)
    {
        std::filesystem::create_directories(path);
        std::ofstream manifest{ path / L"provider.json", std::ios::binary | std::ios::trunc };
        manifest << R"({
            "schemaVersion": 1,
            "id": ")" << providerId << R"(",
            "displayName": "App Extension Test",
            "publisher": "Tests",
            "version": "1.0.0",
            "protocol": { "minVersion": 1, "maxVersion": 1 },
            "runtime": {
                "type": "nativeV1",
                "entrypoint": "provider.exe",
                "arguments": []
            },
            "activationEvents": ["onManualRefresh"],
            "fields": [
                { "id": "value", "displayName": "Value", "type": "string", "defaultVisible": true }
            ]
        })";
    }

    void RichTabProviderAppExtensionCatalogTests::DiscoversValidCandidateWithoutActivatingIt()
    {
        TestDirectory directory;
        WriteManifest(directory.root, "io.example.git");

        const auto result = AppExtensionProviderCatalog::InspectCandidates(
            { Candidate(directory.root) });

        VERIFY_IS_TRUE(result.diagnostics.empty());
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), result.providers.size());
        const auto& provider = result.providers.front();
        VERIFY_ARE_EQUAL(AppExtensionDiscoveryStatus::Discovered, provider.status);
        VERIFY_IS_TRUE(provider.manifest.has_value());
        VERIFY_ARE_EQUAL(std::string{ "io.example.git" }, provider.manifest->id);
        VERIFY_ARE_EQUAL(std::wstring{ L"Tests.RichTabProvider_123" }, provider.identity.packageFamilyName);
    }

    void RichTabProviderAppExtensionCatalogTests::ReportsInvalidIdentityAndPackageStatus()
    {
        TestDirectory directory;
        auto invalidIdentity = Candidate(directory.root);
        invalidIdentity.identity.publisherId.clear();

        const auto result = AppExtensionProviderCatalog::InspectCandidates(
            { std::move(invalidIdentity), Candidate(directory.root, L"unhealthy", false) });

        VERIFY_ARE_EQUAL(static_cast<size_t>(2), result.providers.size());
        VERIFY_ARE_EQUAL(
            AppExtensionDiscoveryStatus::InvalidPackageIdentity,
            result.providers[0].status);
        VERIFY_ARE_EQUAL(
            AppExtensionDiscoveryStatus::PackageUnhealthy,
            result.providers[1].status);
    }

    void RichTabProviderAppExtensionCatalogTests::ReportsMissingAndInvalidManifest()
    {
        TestDirectory directory;
        const auto missingRoot = directory.root / L"missing";
        const auto invalidRoot = directory.root / L"invalid";
        std::filesystem::create_directories(missingRoot);
        std::filesystem::create_directories(invalidRoot);
        {
            std::ofstream manifest{ invalidRoot / L"provider.json" };
            manifest << "{}";
        }

        const auto result = AppExtensionProviderCatalog::InspectCandidates(
            {
                Candidate(missingRoot, L"missing"),
                Candidate(invalidRoot, L"invalid"),
            });

        VERIFY_ARE_EQUAL(static_cast<size_t>(2), result.providers.size());
        VERIFY_ARE_EQUAL(
            AppExtensionDiscoveryStatus::ManifestUnavailable,
            result.providers[0].status);
        VERIFY_ARE_EQUAL(
            AppExtensionDiscoveryStatus::ManifestInvalid,
            result.providers[1].status);
    }

    void RichTabProviderAppExtensionCatalogTests::RejectsDuplicateProviderIds()
    {
        TestDirectory directory;
        const auto first = directory.root / L"first";
        const auto second = directory.root / L"second";
        WriteManifest(first, "io.example.duplicate");
        WriteManifest(second, "io.example.duplicate");

        const auto result = AppExtensionProviderCatalog::InspectCandidates(
            {
                Candidate(first, L"first"),
                Candidate(second, L"second"),
            });

        VERIFY_ARE_EQUAL(static_cast<size_t>(2), result.providers.size());
        VERIFY_ARE_EQUAL(
            AppExtensionDiscoveryStatus::DuplicateProviderId,
            result.providers[0].status);
        VERIFY_ARE_EQUAL(
            AppExtensionDiscoveryStatus::DuplicateProviderId,
            result.providers[1].status);
    }
}
