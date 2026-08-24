// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include <json/json.h>

#include "../RichTabProvider/RichTabDiagnostics.h"

using namespace Microsoft::Terminal::RichTab::Provider;

namespace TerminalAppUnitTests
{
    class RichTabDiagnosticsTests
    {
        TEST_CLASS(RichTabDiagnosticsTests);

        TEST_METHOD(CapturesStructuredEvents);
        TEST_METHOD(SerializesStablePrivateJson);
    };

    void RichTabDiagnosticsTests::CapturesStructuredEvents()
    {
        std::vector<RichTabDiagnosticEventData> events;
        {
            ScopedRichTabDiagnosticObserver observer{ [&](const auto& event) {
                events.emplace_back(event);
            } };
            EmitRichTabDiagnostic({
                .event = RichTabDiagnosticEvent::RefreshPlan,
                .state = RichTabDiagnosticState::Planned,
                .reason = RichTabDiagnosticReason::ContextChanged,
                .sessionId = "session-secret",
                .eligibleCount = size_t{ 2 },
                .startedCount = size_t{ 1 },
            });
        }

        VERIFY_ARE_EQUAL(size_t{ 1 }, events.size());
        VERIFY_ARE_EQUAL(RichTabDiagnosticEvent::RefreshPlan, events[0].event);
        VERIFY_ARE_EQUAL(RichTabDiagnosticState::Planned, events[0].state);
        VERIFY_ARE_EQUAL(RichTabDiagnosticReason::ContextChanged, events[0].reason);
        VERIFY_ARE_EQUAL(std::string{ "session-secret" }, events[0].sessionId);
        VERIFY_ARE_EQUAL(size_t{ 2 }, events[0].eligibleCount.value());
        VERIFY_ARE_EQUAL(size_t{ 1 }, events[0].startedCount.value());
    }

    void RichTabDiagnosticsTests::SerializesStablePrivateJson()
    {
        static constexpr std::string_view sessionSecret{ "private-session-guid" };
        static constexpr std::string_view providerSecret{ "private.company.provider" };
        static constexpr std::string_view tabSecret{ "private-tab-guid" };
        const auto line = SerializeRichTabDiagnosticForTests({
            .level = RichTabDiagnosticLevel::Warning,
            .event = RichTabDiagnosticEvent::PublishResult,
            .state = RichTabDiagnosticState::Rejected,
            .reason = RichTabDiagnosticReason::LeaseStale,
            .sessionId = std::string{ sessionSecret },
            .providerId = std::string{ providerSecret },
            .requestId = "123-4",
            .supersededByRequestId = "123-5",
            .tabId = std::string{ tabSecret },
            .sessionIncarnation = uint64_t{ 2 },
            .contextRevision = uint64_t{ 7 },
            .catalogRevision = uint64_t{ 8 },
            .generation = uint64_t{ 9 },
            .effectiveCount = size_t{ 4 },
            .filterMatchedCount = size_t{ 2 },
            .fieldCount = size_t{ 3 },
            .persistent = true,
            .presentationPresent = false,
        });

        VERIFY_IS_TRUE(line.ends_with('\n'));
        VERIFY_IS_TRUE(line.find(sessionSecret) == std::string::npos);
        VERIFY_IS_TRUE(line.find(providerSecret) == std::string::npos);
        VERIFY_IS_TRUE(line.find(tabSecret) == std::string::npos);

        Json::CharReaderBuilder builder;
        Json::Value root;
        std::string error;
        std::istringstream stream{ line };
        VERIFY_IS_TRUE(Json::parseFromStream(builder, stream, &root, &error));
        VERIFY_ARE_EQUAL(1, root["schema"].asInt());
        VERIFY_ARE_EQUAL(std::string{ "warn" }, root["level"].asString());
        VERIFY_ARE_EQUAL(std::string{ "publish_result" }, root["event"].asString());
        VERIFY_ARE_EQUAL(std::string{ "rejected" }, root["state"].asString());
        VERIFY_ARE_EQUAL(std::string{ "lease_stale" }, root["reason"].asString());
        VERIFY_ARE_EQUAL(std::string{ "123-5" }, root["superseded_by_request"].asString());
        VERIFY_ARE_EQUAL(Json::UInt64{ 2 }, root["session_incarnation"].asUInt64());
        VERIFY_ARE_EQUAL(Json::UInt64{ 7 }, root["context_revision"].asUInt64());
        VERIFY_ARE_EQUAL(Json::UInt64{ 8 }, root["catalog_revision"].asUInt64());
        VERIFY_ARE_EQUAL(Json::UInt64{ 9 }, root["generation"].asUInt64());
        VERIFY_ARE_EQUAL(Json::UInt64{ 4 }, root["effective_count"].asUInt64());
        VERIFY_ARE_EQUAL(Json::UInt64{ 2 }, root["filter_matched_count"].asUInt64());
        VERIFY_ARE_EQUAL(Json::UInt64{ 3 }, root["field_count"].asUInt64());
        VERIFY_IS_TRUE(root["persistent"].asBool());
        VERIFY_IS_FALSE(root["presentation_present"].asBool());
        VERIFY_IS_TRUE(root["session"].asString().starts_with('s'));
        VERIFY_IS_TRUE(root["provider"].asString().starts_with('p'));
        VERIFY_IS_TRUE(root["tab"].asString().starts_with('t'));
    }
}
