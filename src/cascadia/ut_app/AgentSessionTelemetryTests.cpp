// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/AgentSessionTelemetry.h"

using namespace WEX::TestExecution;
namespace Telemetry = TerminalApp::AgentSessionTelemetry;

namespace TerminalAppUnitTests
{
    class AgentSessionTelemetryTests
    {
        TEST_CLASS(AgentSessionTelemetryTests);
        TEST_METHOD(ParsesSessionSettings);
        TEST_METHOD(BucketsPrivateValues);
        TEST_METHOD(RejectsIncompleteSnapshots);
    };

    static Json::Value Snapshot()
    {
        Json::Value value{ Json::objectValue };
        value["start_id"] = "0ba8a608-26c5-40a2-8020-5a1cd873f251";
        value["session_id"] = "session";
        value["start_kind"] = "Load";
        value["agent_id"] = "copilot";
        value["agent_source"] = "wsl";
        value["delegate_agent_id"] = "claude";
        value["model_source"] = "byok";
        value["autofix_enabled"] = false;
        value["automatic_yolo"] = Json::nullValue;
        value["yolo_policy_blocked"] = false;
        value["yolo_control_owner"] = "provider-restored";
        return value;
    }

    void AgentSessionTelemetryTests::ParsesSessionSettings()
    {
        const auto parsed = Telemetry::Parse(Snapshot());
        VERIFY_IS_TRUE(parsed.has_value());
        VERIFY_ARE_EQUAL(std::string{ "Load" }, std::string{ parsed->kind });
        VERIFY_ARE_EQUAL(std::string{ "copilot" }, std::string{ parsed->agentId });
        VERIFY_ARE_EQUAL(std::string{ "byok" }, std::string{ parsed->modelSource });
        VERIFY_ARE_EQUAL(std::string{ "provider" }, std::string{ parsed->automaticYolo });
        VERIFY_IS_FALSE(parsed->autofix);
    }

    void AgentSessionTelemetryTests::BucketsPrivateValues()
    {
        auto value = Snapshot();
        for (const auto field : { "agent_id", "delegate_agent_id", "agent_source", "model_source", "yolo_control_owner" })
        {
            value[field] = "custom:private-path-or-command";
        }
        const auto parsed = Telemetry::Parse(value);
        VERIFY_IS_TRUE(parsed.has_value());
        VERIFY_ARE_EQUAL(std::string{ "custom" }, std::string{ parsed->agentId });
        VERIFY_ARE_EQUAL(std::string{ "custom" }, std::string{ parsed->delegateAgentId });
        VERIFY_ARE_EQUAL(std::string{ "unknown" }, std::string{ parsed->source });
        VERIFY_ARE_EQUAL(std::string{ "unknown" }, std::string{ parsed->modelSource });
        VERIFY_ARE_EQUAL(std::string{ "unknown" }, std::string{ parsed->yoloControlOwner });
        VERIFY_ARE_EQUAL(std::wstring{ L"unknown" }, std::wstring{ Telemetry::Bucket<wchar_t>(L"private-value", { L"auto", L"prompt" }, L"unknown") });
    }

    void AgentSessionTelemetryTests::RejectsIncompleteSnapshots()
    {
        const auto original = Snapshot();
        for (const auto& field : original.getMemberNames())
        {
            auto value = original;
            value.removeMember(field);
            VERIFY_IS_FALSE(Telemetry::Parse(value).has_value());
        }
        auto value = original;
        value["start_kind"] = "Failed";
        VERIFY_IS_FALSE(Telemetry::Parse(value).has_value());
        value = original;
        value["autofix_enabled"] = "false";
        VERIFY_IS_FALSE(Telemetry::Parse(value).has_value());
    }
}
