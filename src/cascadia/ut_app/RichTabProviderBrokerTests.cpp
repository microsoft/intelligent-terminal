// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../RichTabProvider/ProviderBroker.h"

using namespace Microsoft::Terminal::RichTab::Provider;

namespace TerminalAppUnitTests
{
    class RichTabProviderBrokerTests
    {
        TEST_CLASS(RichTabProviderBrokerTests);

        TEST_METHOD(ComposesDeclaredVisibleFields);
        TEST_METHOD(ComposesConfiguredFieldsInOrder);
        TEST_METHOD(ExplicitlyEmptyFieldsHideProviderMetadata);
        TEST_METHOD(EmptySnapshotsClearPresentation);
        TEST_METHOD(CatalogEnforcesSourcePrecedenceAndConsent);
    };

    void RichTabProviderBrokerTests::ComposesDeclaredVisibleFields()
    {
        Registration provider;
        provider.manifest.id = "tests.presentation";
        provider.manifest.fields = {
            FieldDeclaration{ "name", "Name", FieldType::String, true },
            FieldDeclaration{ "hidden", "Hidden", FieldType::String, false },
            FieldDeclaration{ "count", "Count", FieldType::Integer, true },
        };

        Snapshot snapshot;
        snapshot.fields.emplace("name", std::string{ "PR #42" });
        snapshot.fields.emplace("hidden", std::string{ "not shown" });
        snapshot.fields.emplace("count", int64_t{ 3 });
        snapshot.tooltip = "https://example.test/pr/42";
        snapshot.accessibilityText = "Pull request 42, three checks";

        const auto presentation = ProviderBroker::ComposePresentation(
            { provider },
            { { provider.manifest.id, snapshot } });

        VERIFY_IS_TRUE(presentation.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"PR #42 \u00b7 3" }, presentation->text);
        VERIFY_ARE_EQUAL(std::wstring{ L"https://example.test/pr/42" }, presentation->tooltip);
        VERIFY_ARE_EQUAL(std::wstring{ L"Pull request 42, three checks" }, presentation->accessibilityText);
    }

    void RichTabProviderBrokerTests::ComposesConfiguredFieldsInOrder()
    {
        Registration provider;
        provider.manifest.id = "tests.presentation";
        provider.manifest.fields = {
            FieldDeclaration{ "name", "Name", FieldType::String, true },
            FieldDeclaration{ "hidden", "Hidden", FieldType::String, false },
            FieldDeclaration{ "count", "Count", FieldType::Integer, true },
        };

        Snapshot snapshot;
        snapshot.fields.emplace("name", std::string{ "PR #42" });
        snapshot.fields.emplace("hidden", std::string{ "approved" });
        snapshot.fields.emplace("count", int64_t{ 3 });

        const auto presentation = ProviderBroker::ComposePresentation(
            { provider },
            { { provider.manifest.id, snapshot } },
            { ProviderPreference{
                provider.manifest.id,
                std::nullopt,
                std::vector<std::string>{ "count", "unknown", "hidden" } } });

        VERIFY_IS_TRUE(presentation.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"3 \u00b7 approved" }, presentation->text);
    }

    void RichTabProviderBrokerTests::ExplicitlyEmptyFieldsHideProviderMetadata()
    {
        Registration hiddenProvider;
        hiddenProvider.manifest.id = "tests.hidden";
        hiddenProvider.manifest.fields = {
            FieldDeclaration{ "value", "Value", FieldType::String, true },
        };
        Registration visibleProvider;
        visibleProvider.manifest.id = "tests.visible";
        visibleProvider.manifest.fields = hiddenProvider.manifest.fields;

        Snapshot hiddenSnapshot;
        hiddenSnapshot.fields.emplace("value", std::string{ "hidden" });
        hiddenSnapshot.tooltip = "hidden tooltip";
        hiddenSnapshot.accessibilityText = "hidden accessibility";
        Snapshot visibleSnapshot;
        visibleSnapshot.fields.emplace("value", std::string{ "visible" });
        visibleSnapshot.tooltip = "visible tooltip";
        visibleSnapshot.accessibilityText = "visible accessibility";

        const auto presentation = ProviderBroker::ComposePresentation(
            { hiddenProvider, visibleProvider },
            {
                { hiddenProvider.manifest.id, hiddenSnapshot },
                { visibleProvider.manifest.id, visibleSnapshot },
            },
            { ProviderPreference{
                hiddenProvider.manifest.id,
                std::nullopt,
                std::vector<std::string>{} } });

        VERIFY_IS_TRUE(presentation.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"visible" }, presentation->text);
        VERIFY_ARE_EQUAL(std::wstring{ L"visible tooltip" }, presentation->tooltip);
        VERIFY_ARE_EQUAL(std::wstring{ L"visible accessibility" }, presentation->accessibilityText);
    }

    void RichTabProviderBrokerTests::EmptySnapshotsClearPresentation()
    {
        Registration provider;
        provider.manifest.id = "tests.empty";
        provider.manifest.fields = {
            FieldDeclaration{ "value", "Value", FieldType::String, true },
        };

        Snapshot snapshot;
        const auto presentation = ProviderBroker::ComposePresentation(
            { provider },
            { { provider.manifest.id, snapshot } });

        VERIFY_IS_FALSE(presentation.has_value());
    }

    void RichTabProviderBrokerTests::CatalogEnforcesSourcePrecedenceAndConsent()
    {
        const auto makeProvider = [](const std::string& id,
                                     ProviderSourceIdentity identity,
                                     const bool enabled) {
            Registration registration;
            registration.manifest.id = id;
            registration.manifest.displayName = id;
            registration.enabled = enabled;
            registration.integrityValid = true;
            registration.sourceIdentity = std::move(identity);
            return registration;
        };

        auto development = makeProvider(
            "tests.shadowed",
            DevelopmentSourceIdentity("tests.shadowed", LR"(C:\dev)"),
            true);
        auto appExtension = makeProvider(
            "tests.shadowed",
            AppExtensionSourceIdentity(
                "tests.shadowed",
                L"rich-tabs",
                L"Tests.Provider_123",
                L"Tests.Provider_1.0.0.0_x64__123",
                L"publisher123",
                L"1.0.0.0"),
            false);
        auto legacy = makeProvider(
            "tests.legacy",
            LegacyManagedSourceIdentity("tests.legacy", LR"(C:\managed)"),
            true);

        const auto catalog = ProviderBroker::BuildCatalog(
            { std::move(development), std::move(appExtension), std::move(legacy) });

        VERIFY_IS_TRUE(catalog.available.empty());
        VERIFY_ARE_EQUAL(static_cast<size_t>(3), catalog.descriptors.size());
        const auto app = std::find_if(
            catalog.descriptors.begin(),
            catalog.descriptors.end(),
            [](const auto& descriptor) {
                return descriptor.source == ProviderSourceKind::AppExtension;
            });
        const auto dev = std::find_if(
            catalog.descriptors.begin(),
            catalog.descriptors.end(),
            [](const auto& descriptor) {
                return descriptor.source == ProviderSourceKind::Development;
            });
        const auto managed = std::find_if(
            catalog.descriptors.begin(),
            catalog.descriptors.end(),
            [](const auto& descriptor) {
                return descriptor.source == ProviderSourceKind::LegacyManaged;
            });
        VERIFY_IS_TRUE(app != catalog.descriptors.end());
        VERIFY_IS_TRUE(dev != catalog.descriptors.end());
        VERIFY_IS_TRUE(managed != catalog.descriptors.end());
        VERIFY_IS_FALSE(app->consentEnabled);
        VERIFY_IS_FALSE(app->shadowed);
        VERIFY_ARE_EQUAL(
            ProviderConsentKey(AppExtensionSourceIdentity(
                "tests.shadowed",
                L"rich-tabs",
                L"Tests.Provider_123",
                L"Tests.Provider_1.0.0.0_x64__123",
                L"publisher123",
                L"1.0.0.0"))
                .value(),
            app->consentKey);
        VERIFY_IS_TRUE(dev->shadowed);
        VERIFY_IS_FALSE(managed->eligible);
        VERIFY_IS_TRUE(managed->consentKey.empty());
    }
}
