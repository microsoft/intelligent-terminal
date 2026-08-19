// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "RichTabProviderPreference.g.h"
#include "JsonUtils.h"

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    struct RichTabProviderPreference : RichTabProviderPreferenceT<RichTabProviderPreference>
    {
        RichTabProviderPreference(winrt::hstring id);

        Model::RichTabProviderPreference Copy() const;
        Json::Value ToJson() const;
        static Model::RichTabProviderPreference FromJson(const Json::Value& json);

        WINRT_PROPERTY(winrt::hstring, Id);
        WINRT_PROPERTY(winrt::Windows::Foundation::IReference<bool>, Enabled, nullptr);
        WINRT_PROPERTY(
            winrt::Windows::Foundation::Collections::IVector<winrt::hstring>,
            Fields,
            nullptr);
    };
}

namespace Microsoft::Terminal::Settings::Model::JsonUtils
{
    template<>
    struct ConversionTrait<winrt::Microsoft::Terminal::Settings::Model::RichTabProviderPreference>
    {
        winrt::Microsoft::Terminal::Settings::Model::RichTabProviderPreference FromJson(const Json::Value& json)
        {
            return winrt::Microsoft::Terminal::Settings::Model::implementation::RichTabProviderPreference::FromJson(json);
        }

        bool CanConvert(const Json::Value& json) const
        {
            return json.isObject();
        }

        Json::Value ToJson(const winrt::Microsoft::Terminal::Settings::Model::RichTabProviderPreference& value)
        {
            return winrt::get_self<winrt::Microsoft::Terminal::Settings::Model::implementation::RichTabProviderPreference>(value)->ToJson();
        }

        std::string TypeDescription() const
        {
            return "RichTabProviderPreference";
        }
    };
}

namespace winrt::Microsoft::Terminal::Settings::Model::factory_implementation
{
    BASIC_FACTORY(RichTabProviderPreference);
}
