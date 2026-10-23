// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "CustomAgent.h"
#include "CustomAgent.g.cpp"

namespace CustomAgents = ::Microsoft::Terminal::CustomAgents;
using namespace Microsoft::Terminal::Settings::Model;

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    void CustomAgent::Validate() const
    {
        THROW_HR_IF_MSG(E_INVALIDARG, !CustomAgents::IsId(_Id), "Invalid custom agent ID");
        THROW_HR_IF_MSG(E_INVALIDARG, _AcpCommand.empty() && _CliCommand.empty(), "A custom agent requires an ACP or interactive CLI command");
        if (!_AcpCommand.empty())
        {
            CustomAgents::Parse(_AcpCommand);
        }
        if (!_CliCommand.empty())
        {
            CustomAgents::Parse(_CliCommand);
        }
        if (!_ResumeCommand.empty())
        {
            THROW_HR_IF_MSG(E_INVALIDARG, _CliCommand.empty(), "A custom resume command requires an interactive CLI command");
            CustomAgents::Parse(_ResumeCommand, true);
        }
        THROW_HR_IF_MSG(E_INVALIDARG, _CanResumeAcpSessions && _ResumeCommand.empty(), "Shared ACP session storage requires a custom resume command");
    }

    Model::CustomAgent CustomAgent::Copy() const
    {
        auto copy = winrt::make_self<CustomAgent>(_Id, _Name);
        copy->_AcpCommand = _AcpCommand;
        copy->_CliCommand = _CliCommand;
        copy->_ResumeCommand = _ResumeCommand;
        copy->_CanResumeAcpSessions = _CanResumeAcpSessions;
        return *copy;
    }

    Json::Value CustomAgent::ToJson() const
    {
        Validate();
        Json::Value json{ Json::objectValue };
        JsonUtils::SetValueForKey(json, "id", _Id);
        JsonUtils::SetValueForKey(json, "name", _Name);
        if (!_AcpCommand.empty())
        {
            json["acp"] = CustomAgents::ToJson(CustomAgents::Parse(_AcpCommand));
        }
        if (!_CliCommand.empty())
        {
            json["cli"] = CustomAgents::ToJson(CustomAgents::Parse(_CliCommand));
        }
        if (!_ResumeCommand.empty())
        {
            json["resume"] = CustomAgents::ToJson(CustomAgents::Parse(_ResumeCommand, true));
            json["resume"]["canResumeAcpSessions"] = _CanResumeAcpSessions;
        }
        return json;
    }

    Model::CustomAgent CustomAgent::FromJson(const Json::Value& json)
    {
        const auto id = JsonUtils::GetValueForKey<winrt::hstring>(json, "id");
        const auto name = JsonUtils::GetValueForKey<winrt::hstring>(json, "name");
        auto agent = winrt::make_self<CustomAgent>(id, name.empty() ? id : name);
        if (json.isMember("acp"))
        {
            agent->_AcpCommand = CustomAgents::Format(CustomAgents::FromJson(json["acp"]));
        }
        if (json.isMember("cli"))
        {
            agent->_CliCommand = CustomAgents::Format(CustomAgents::FromJson(json["cli"]));
        }
        if (json.isMember("resume"))
        {
            agent->_ResumeCommand = CustomAgents::Format(CustomAgents::FromJson(json["resume"], true));
            JsonUtils::GetValueForKey(json["resume"], "canResumeAcpSessions", agent->_CanResumeAcpSessions);
        }
        agent->Validate();
        return *agent;
    }
}
