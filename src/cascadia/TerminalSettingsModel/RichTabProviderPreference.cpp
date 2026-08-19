// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "RichTabProviderPreference.h"
#include "RichTabProviderPreference.g.cpp"

using namespace Microsoft::Terminal::Settings::Model;

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    RichTabProviderPreference::RichTabProviderPreference(winrt::hstring id) :
        _Id{ std::move(id) }
    {
    }

    Model::RichTabProviderPreference RichTabProviderPreference::Copy() const
    {
        auto copy = winrt::make_self<RichTabProviderPreference>(_Id);
        copy->_Enabled = _Enabled;
        if (_Fields)
        {
            copy->_Fields = winrt::single_threaded_vector<winrt::hstring>();
            for (const auto& field : _Fields)
            {
                copy->_Fields.Append(field);
            }
        }
        return *copy;
    }

    Json::Value RichTabProviderPreference::ToJson() const
    {
        Json::Value json{ Json::objectValue };
        JsonUtils::SetValueForKey(json, "id", _Id);
        if (_Enabled)
        {
            JsonUtils::SetValueForKey(json, "enabled", _Enabled.Value());
        }
        if (_Fields)
        {
            JsonUtils::SetValueForKey(json, "fields", _Fields);
        }
        return json;
    }

    Model::RichTabProviderPreference RichTabProviderPreference::FromJson(const Json::Value& json)
    {
        const auto id = JsonUtils::GetValueForKey<winrt::hstring>(json, "id");
        auto preference = winrt::make_self<RichTabProviderPreference>(id);
        if (json.isMember("enabled"))
        {
            const auto enabled = JsonUtils::GetValueForKey<bool>(json, "enabled");
            preference->_Enabled = winrt::box_value(enabled).as<winrt::Windows::Foundation::IReference<bool>>();
        }
        if (json.isMember("fields"))
        {
            JsonUtils::GetValueForKey(json, "fields", preference->_Fields);
        }
        return *preference;
    }
}
