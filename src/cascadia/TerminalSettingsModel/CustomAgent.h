// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "CustomAgent.g.h"
#include "JsonUtils.h"
#include "../inc/CustomAgentUtils.h"

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    struct CustomAgent : CustomAgentT<CustomAgent>
    {
        CustomAgent(winrt::hstring id, winrt::hstring name) :
            _Id{ std::move(id) }, _Name{ std::move(name) }
        {
        }

        Model::CustomAgent Copy() const;
        Json::Value ToJson() const;
        static Model::CustomAgent FromJson(const Json::Value& json);
        void Validate() const;

        WINRT_PROPERTY(winrt::hstring, Id);
        WINRT_PROPERTY(winrt::hstring, Name);
        WINRT_PROPERTY(winrt::hstring, AcpCommand);
        WINRT_PROPERTY(winrt::hstring, CliCommand);
        WINRT_PROPERTY(winrt::hstring, ResumeCommand);
        WINRT_PROPERTY(bool, CanResumeAcpSessions, false);
    };
}

namespace Microsoft::Terminal::Settings::Model::JsonUtils
{
    template<>
    struct ConversionTrait<winrt::Microsoft::Terminal::Settings::Model::CustomAgent>
    {
        auto FromJson(const Json::Value& json)
        {
            return winrt::Microsoft::Terminal::Settings::Model::implementation::CustomAgent::FromJson(json);
        }
        bool CanConvert(const Json::Value& json) const { return json.isObject(); }
        Json::Value ToJson(const winrt::Microsoft::Terminal::Settings::Model::CustomAgent& value)
        {
            return winrt::get_self<winrt::Microsoft::Terminal::Settings::Model::implementation::CustomAgent>(value)->ToJson();
        }
        std::string TypeDescription() const { return "CustomAgent"; }
    };
}

namespace winrt::Microsoft::Terminal::Settings::Model::factory_implementation
{
    BASIC_FACTORY(CustomAgent);
}
