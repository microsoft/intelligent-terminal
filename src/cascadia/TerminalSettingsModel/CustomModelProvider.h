// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "CustomModel.g.h"
#include "CustomModelProvider.g.h"
#include "JsonUtils.h"

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    struct CustomModel : CustomModelT<CustomModel>
    {
        CustomModel(winrt::hstring id, winrt::hstring name) :
            _Id{ std::move(id) },
            _Name{ std::move(name) }
        {
        }

        Model::CustomModel Copy() const;
        Json::Value ToJson() const;
        static Model::CustomModel FromJson(const Json::Value& json);

        WINRT_PROPERTY(winrt::hstring, Id);
        WINRT_PROPERTY(winrt::hstring, Name);
    };

    struct CustomModelProvider : CustomModelProviderT<CustomModelProvider>
    {
        CustomModelProvider(winrt::hstring id, winrt::hstring name, winrt::hstring baseUrl);

        Model::CustomModelProvider Copy() const;
        Json::Value ToJson() const;
        static Model::CustomModelProvider FromJson(const Json::Value& json);

        static winrt::hstring SelectionId(const winrt::hstring& providerId, const winrt::hstring& modelId);
        static bool TryParseSelectionId(std::wstring_view selectionId, std::wstring& providerId, std::wstring& modelId);
        static winrt::hstring ResolvedLocation(const Model::CustomModelProvider& provider);

        WINRT_PROPERTY(winrt::hstring, Id);
        WINRT_PROPERTY(winrt::hstring, Name);
        WINRT_PROPERTY(winrt::hstring, BaseUrl);
        WINRT_PROPERTY(winrt::hstring, ApiContract, L"openai-compatible");
        WINRT_PROPERTY(winrt::hstring, Location, L"auto");
        WINRT_PROPERTY(winrt::hstring, ApiKeyCredential);
        WINRT_PROPERTY(bool, ApiKeyRequired, false);
        WINRT_PROPERTY(
            winrt::Windows::Foundation::Collections::IVector<Model::CustomModel>,
            Models,
            winrt::single_threaded_vector<Model::CustomModel>());
    };
}

namespace Microsoft::Terminal::Settings::Model::JsonUtils
{
    template<>
    struct ConversionTrait<winrt::Microsoft::Terminal::Settings::Model::CustomModel>
    {
        winrt::Microsoft::Terminal::Settings::Model::CustomModel FromJson(const Json::Value& json)
        {
            return winrt::Microsoft::Terminal::Settings::Model::implementation::CustomModel::FromJson(json);
        }

        bool CanConvert(const Json::Value& json) const
        {
            return json.isObject();
        }

        Json::Value ToJson(const winrt::Microsoft::Terminal::Settings::Model::CustomModel& value)
        {
            return winrt::get_self<winrt::Microsoft::Terminal::Settings::Model::implementation::CustomModel>(value)->ToJson();
        }

        std::string TypeDescription() const
        {
            return "CustomModel";
        }
    };

    template<>
    struct ConversionTrait<winrt::Microsoft::Terminal::Settings::Model::CustomModelProvider>
    {
        winrt::Microsoft::Terminal::Settings::Model::CustomModelProvider FromJson(const Json::Value& json)
        {
            return winrt::Microsoft::Terminal::Settings::Model::implementation::CustomModelProvider::FromJson(json);
        }

        bool CanConvert(const Json::Value& json) const
        {
            return json.isObject();
        }

        Json::Value ToJson(const winrt::Microsoft::Terminal::Settings::Model::CustomModelProvider& value)
        {
            return winrt::get_self<winrt::Microsoft::Terminal::Settings::Model::implementation::CustomModelProvider>(value)->ToJson();
        }

        std::string TypeDescription() const
        {
            return "CustomModelProvider";
        }
    };
}

namespace winrt::Microsoft::Terminal::Settings::Model::factory_implementation
{
    BASIC_FACTORY(CustomModel);
    BASIC_FACTORY(CustomModelProvider);
}
