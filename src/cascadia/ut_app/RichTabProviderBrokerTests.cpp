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
        TEST_METHOD(EmptySnapshotsClearPresentation);
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
}
