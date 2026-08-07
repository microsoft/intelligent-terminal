// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../RichTabProvider/ProviderRegistry.h"
#include "../RichTabProvider/BuiltInProviderCatalog.h"

#include <fstream>

using namespace Microsoft::Terminal::RichTab::Provider;
using namespace WEX::TestExecution;
using namespace WEX::Logging;

namespace TerminalAppUnitTests
{
    class RichTabProviderRegistryTests
    {
        TEST_CLASS(RichTabProviderRegistryTests);

        TEST_METHOD(ManagedProviderLifecycleAndIntegrity);
        TEST_METHOD(ChangedManagedUpdateIsDisabled);
        TEST_METHOD(DevelopmentRegistrationRemainsMutable);
        TEST_METHOD(InterruptedManagedUpdateRollsBack);
        TEST_METHOD(PayloadHashFramingIsUnambiguous);
        TEST_METHOD(RecoveryReadFailurePreservesBothPayloads);
        TEST_METHOD(InterruptedRemovalCompletes);
        TEST_METHOD(ManagedRegistrationCannotEscapeRegistry);
        TEST_METHOD(BrokenProviderCanBeDisabled);
        TEST_METHOD(FailedDevelopmentRegistrationIsNotCommitted);
        TEST_METHOD(MissingBuiltInCatalogIsEmpty);
        TEST_METHOD(LoadsTrustedBuiltInProvider);
        TEST_METHOD(RejectsUnexpectedBuiltInProviderId);

        struct TestDirectories
        {
            std::filesystem::path root;
            std::filesystem::path source;
            std::filesystem::path registry;

            TestDirectories()
            {
                root = std::filesystem::temp_directory_path() /
                       (L"rich-tab-registry-test-" +
                        std::to_wstring(GetCurrentProcessId()) +
                        L"-" +
                        std::to_wstring(GetTickCount64()));
                source = root / L"source";
                registry = root / L"registry";
                std::filesystem::create_directories(source);
            }

            ~TestDirectories()
            {
                std::error_code error;
                std::filesystem::remove_all(root, error);
            }
        };

        static void WriteProvider(
            const std::filesystem::path& root,
            std::string_view id,
            std::string_view script);
    };

    void RichTabProviderRegistryTests::WriteProvider(
        const std::filesystem::path& root,
        const std::string_view id,
        const std::string_view script)
    {
        std::filesystem::create_directories(root);
        std::ofstream manifest{ root / L"provider.json", std::ios::binary | std::ios::trunc };
        manifest << R"({
            "schemaVersion": 1,
            "id": ")" << id << R"(",
            "displayName": "Registry Test",
            "publisher": "Tests",
            "version": "1.0.0",
            "protocol": { "minVersion": 1, "maxVersion": 1 },
            "runtime": {
                "type": "powerShellV1",
                "entrypoint": "provider.ps1",
                "arguments": []
            },
            "activationEvents": ["onManualRefresh"],
            "fields": [
                { "id": "value", "displayName": "Value", "type": "string", "defaultVisible": true }
            ]
        })";
        manifest.close();

        std::ofstream provider{ root / L"provider.ps1", std::ios::binary | std::ios::trunc };
        provider << script;
    }

    void RichTabProviderRegistryTests::MissingBuiltInCatalogIsEmpty()
    {
        TestDirectories directories;
        const auto loaded = BuiltInProviderCatalog::Load(directories.root);

        VERIFY_IS_TRUE(loaded.value.has_value());
        VERIFY_IS_TRUE(loaded.value->empty());
        VERIFY_IS_TRUE(loaded.errors.empty());
    }

    void RichTabProviderRegistryTests::LoadsTrustedBuiltInProvider()
    {
        TestDirectories directories;
        WriteProvider(
            directories.root / L"RichTabProviders" / L"GitStatus",
            "com.microsoft.intelligent-terminal.git-status",
            "Write-Output '{}'");

        const auto loaded = BuiltInProviderCatalog::Load(directories.root);
        VERIFY_IS_TRUE(loaded.value.has_value());
        VERIFY_IS_TRUE(loaded.errors.empty());
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), loaded.value->size());

        const auto& registration = loaded.value->front();
        VERIFY_ARE_EQUAL(std::string{ "com.microsoft.intelligent-terminal.git-status" }, registration.manifest.id);
        VERIFY_ARE_EQUAL(RegistrationKind::Managed, registration.kind);
        VERIFY_IS_TRUE(registration.enabled);
        VERIFY_IS_TRUE(registration.integrityValid);
    }

    void RichTabProviderRegistryTests::RejectsUnexpectedBuiltInProviderId()
    {
        TestDirectories directories;
        WriteProvider(
            directories.root / L"RichTabProviders" / L"GitStatus",
            "sample.git-status",
            "Write-Output '{}'");

        const auto loaded = BuiltInProviderCatalog::Load(directories.root);
        VERIFY_IS_TRUE(loaded.value.has_value());
        VERIFY_IS_TRUE(loaded.value->empty());
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), loaded.errors.size());
    }

    void RichTabProviderRegistryTests::ManagedProviderLifecycleAndIntegrity()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.managed-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");

        ProviderRegistry registry{ directories.registry };
        const auto installed = registry.Install(directories.source / L"provider.json");
        VERIFY_IS_TRUE(installed.value.has_value());
        VERIFY_IS_FALSE(installed.value->enabled);
        VERIFY_IS_TRUE(installed.value->integrityValid);
        VERIFY_ARE_EQUAL(RegistrationKind::Managed, installed.value->kind);
        VERIFY_IS_TRUE(installed.value->root != directories.source);

        const auto enabled = registry.SetEnabled(id, true);
        VERIFY_IS_TRUE(enabled.value.has_value());
        VERIFY_IS_TRUE(enabled.value->enabled);

        std::ofstream tamper{ enabled.value->root / L"provider.ps1", std::ios::binary | std::ios::app };
        tamper << "# changed\n";
        tamper.close();

        const auto listed = registry.List();
        VERIFY_IS_TRUE(listed.value.has_value());
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), listed.value->size());
        VERIFY_IS_FALSE(listed.value->front().integrityValid);
        VERIFY_IS_FALSE(registry.SetEnabled(id, true).value.has_value());

        const auto removed = registry.Remove(id);
        VERIFY_IS_TRUE(removed.value.has_value());
        VERIFY_IS_FALSE(std::filesystem::exists(enabled.value->root));
        VERIFY_IS_TRUE(registry.List().value->empty());
    }

    void RichTabProviderRegistryTests::ChangedManagedUpdateIsDisabled()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.update-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");

        ProviderRegistry registry{ directories.registry };
        VERIFY_IS_TRUE(registry.Install(directories.source / L"provider.json").value.has_value());
        VERIFY_IS_TRUE(registry.SetEnabled(id, true).value.has_value());

        WriteProvider(directories.source, id, "Write-Output 'v2'\n");
        const auto updated = registry.Install(directories.source / L"provider.json");
        VERIFY_IS_TRUE(updated.value.has_value());
        VERIFY_IS_FALSE(updated.value->enabled);
        VERIFY_IS_TRUE(updated.value->integrityValid);
    }

    void RichTabProviderRegistryTests::DevelopmentRegistrationRemainsMutable()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.dev-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");

        ProviderRegistry registry{ directories.registry };
        const auto registered = registry.RegisterDevelopment(directories.source / L"provider.json");
        VERIFY_IS_TRUE(registered.value.has_value());
        VERIFY_ARE_EQUAL(RegistrationKind::Development, registered.value->kind);
        VERIFY_ARE_EQUAL(directories.source.native(), registered.value->root.native());
        VERIFY_IS_TRUE(registry.SetEnabled(id, true).value.has_value());

        WriteProvider(directories.source, id, "Write-Output 'v2'\n");
        const auto listed = registry.List();
        VERIFY_IS_TRUE(listed.value.has_value());
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), listed.value->size());
        VERIFY_IS_TRUE(listed.value->front().enabled);
        VERIFY_IS_TRUE(listed.value->front().integrityValid);

        VERIFY_IS_TRUE(registry.Remove(id).value.has_value());
        VERIFY_IS_TRUE(std::filesystem::exists(directories.source / L"provider.ps1"));
    }

    void RichTabProviderRegistryTests::InterruptedManagedUpdateRollsBack()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.recovery-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");

        ProviderRegistry registry{ directories.registry };
        const auto installed = registry.Install(directories.source / L"provider.json");
        VERIFY_IS_TRUE(installed.value.has_value());

        const auto backup = directories.registry / L"staging" / L"tests.recovery-provider.backup";
        std::filesystem::create_directories(backup.parent_path());
        std::filesystem::rename(installed.value->root, backup);
        WriteProvider(installed.value->root, id, "Write-Output 'interrupted-v2'\n");

        const auto recovered = registry.List();
        VERIFY_IS_TRUE(recovered.value.has_value());
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), recovered.value->size());
        VERIFY_IS_TRUE(recovered.value->front().integrityValid);

        std::ifstream provider{ recovered.value->front().root / L"provider.ps1", std::ios::binary };
        std::string contents{ std::istreambuf_iterator<char>{ provider }, std::istreambuf_iterator<char>{} };
        VERIFY_IS_TRUE(contents.find("v1") != std::string::npos);
        VERIFY_IS_FALSE(std::filesystem::exists(backup));
    }

    void RichTabProviderRegistryTests::PayloadHashFramingIsUnambiguous()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.hash-framing-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");
        {
            std::ofstream file{ directories.source / L"a", std::ios::binary };
            file << "bc";
        }
        {
            std::ofstream file{ directories.source / L"d", std::ios::binary };
            file << "e";
        }

        ProviderRegistry registry{ directories.registry };
        const auto installed = registry.Install(directories.source / L"provider.json");
        VERIFY_IS_TRUE(installed.value.has_value());
        VERIFY_IS_TRUE(registry.SetEnabled(id, true).value.has_value());

        std::filesystem::remove(directories.source / L"a");
        std::filesystem::remove(directories.source / L"d");
        {
            std::ofstream file{ directories.source / L"a", std::ios::binary };
            file << "b";
        }
        {
            std::ofstream file{ directories.source / L"cd", std::ios::binary };
            file << "e";
        }

        const auto updated = registry.Install(directories.source / L"provider.json");
        VERIFY_IS_TRUE(updated.value.has_value());
        VERIFY_IS_FALSE(updated.value->enabled);
        VERIFY_IS_TRUE(updated.value->payloadHash != installed.value->payloadHash);
    }

    void RichTabProviderRegistryTests::RecoveryReadFailurePreservesBothPayloads()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.read-failure-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");

        ProviderRegistry registry{ directories.registry };
        const auto installed = registry.Install(directories.source / L"provider.json");
        VERIFY_IS_TRUE(installed.value.has_value());

        const auto backup = directories.registry / L"staging" / L"tests.read-failure-provider.backup";
        std::filesystem::rename(installed.value->root, backup);
        WriteProvider(installed.value->root, id, "Write-Output 'interrupted-v2'\n");

        wil::unique_hfile lockedFile{ CreateFileW(
            (backup / L"provider.ps1").c_str(),
            GENERIC_READ,
            0,
            nullptr,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            nullptr) };
        VERIFY_IS_TRUE(static_cast<bool>(lockedFile));

        const auto recovery = registry.List();
        VERIFY_IS_TRUE(recovery.value.has_value());
        VERIFY_IS_TRUE(recovery.value->empty());
        VERIFY_IS_FALSE(recovery.errors.empty());
        VERIFY_IS_TRUE(std::filesystem::exists(installed.value->root));
        VERIFY_IS_TRUE(std::filesystem::exists(backup));
    }

    void RichTabProviderRegistryTests::InterruptedRemovalCompletes()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.removal-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");

        ProviderRegistry registry{ directories.registry };
        const auto installed = registry.Install(directories.source / L"provider.json");
        VERIFY_IS_TRUE(installed.value.has_value());

        const auto backup = directories.registry / L"staging" / L"tests.removal-provider.backup";
        const auto registration = directories.registry / L"registrations" / L"tests.removal-provider.json";
        const auto tombstone = directories.registry / L"removals" / L"tests.removal-provider.json";
        std::filesystem::rename(installed.value->root, backup);
        std::filesystem::remove(registration);
        {
            std::ofstream file{ tombstone, std::ios::binary };
            file << R"({"id":"tests.removal-provider"})";
        }

        const auto recovered = registry.List();
        VERIFY_IS_TRUE(recovered.value.has_value());
        VERIFY_IS_TRUE(recovered.value->empty());
        VERIFY_IS_FALSE(std::filesystem::exists(backup));
        VERIFY_IS_FALSE(std::filesystem::exists(tombstone));
    }

    void RichTabProviderRegistryTests::ManagedRegistrationCannotEscapeRegistry()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.root-validation-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");

        ProviderRegistry registry{ directories.registry };
        VERIFY_IS_TRUE(registry.Install(directories.source / L"provider.json").value.has_value());

        const auto registration = directories.registry / L"registrations" / L"tests.root-validation-provider.json";
        Json::Value json;
        {
            std::ifstream file{ registration, std::ios::binary };
            file >> json;
        }
        json["root"] = directories.source.string();
        {
            std::ofstream file{ registration, std::ios::binary | std::ios::trunc };
            file << json;
        }

        const auto listed = registry.List();
        VERIFY_IS_TRUE(listed.value.has_value());
        VERIFY_IS_TRUE(listed.value->empty());
        VERIFY_IS_FALSE(listed.errors.empty());
        VERIFY_IS_TRUE(std::filesystem::exists(directories.source / L"provider.ps1"));
    }

    void RichTabProviderRegistryTests::BrokenProviderCanBeDisabled()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.broken-disable-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");

        ProviderRegistry registry{ directories.registry };
        VERIFY_IS_TRUE(registry.RegisterDevelopment(directories.source / L"provider.json").value.has_value());
        VERIFY_IS_TRUE(registry.SetEnabled(id, true).value.has_value());
        std::filesystem::remove(directories.source / L"provider.json");

        const auto disabled = registry.SetEnabled(id, false);
        VERIFY_IS_TRUE(disabled.value.has_value());
        VERIFY_IS_FALSE(disabled.value->enabled);
        VERIFY_ARE_EQUAL(std::string{ id }, disabled.value->manifest.id);

        WriteProvider(directories.source, id, "Write-Output 'v1'\n");
        const auto listed = registry.List();
        VERIFY_IS_TRUE(listed.value.has_value());
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), listed.value->size());
        VERIFY_IS_FALSE(listed.value->front().enabled);
    }

    void RichTabProviderRegistryTests::FailedDevelopmentRegistrationIsNotCommitted()
    {
        TestDirectories directories;
        static constexpr std::string_view id{ "tests.failed-dev-provider" };
        WriteProvider(directories.source, id, "Write-Output 'v1'\n");
        const auto alternateManifest = directories.source / L"alternate.json";
        std::filesystem::copy_file(directories.source / L"provider.json", alternateManifest);
        {
            std::ofstream manifest{ directories.source / L"provider.json", std::ios::binary | std::ios::trunc };
            manifest << "{}";
        }

        ProviderRegistry registry{ directories.registry };
        const auto registered = registry.RegisterDevelopment(alternateManifest);
        VERIFY_IS_FALSE(registered.value.has_value());
        VERIFY_IS_FALSE(std::filesystem::exists(
            directories.registry / L"registrations" / L"tests.failed-dev-provider.json"));
    }
}
