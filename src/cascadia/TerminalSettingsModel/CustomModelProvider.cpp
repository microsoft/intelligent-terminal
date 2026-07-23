// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "CustomModelProvider.h"
#include "../inc/CustomModelProviderUtils.h"
#include "CustomModel.g.cpp"
#include "CustomModelProvider.g.cpp"

using namespace Microsoft::Terminal::Settings::Model;

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    Model::CustomModel CustomModel::Copy() const
    {
        return winrt::make<CustomModel>(_Id, _Name);
    }

    Json::Value CustomModel::ToJson() const
    {
        Json::Value json{ Json::objectValue };
        JsonUtils::SetValueForKey(json, "id", _Id);
        JsonUtils::SetValueForKey(json, "name", _Name);
        return json;
    }

    Model::CustomModel CustomModel::FromJson(const Json::Value& json)
    {
        const auto id = JsonUtils::GetValueForKey<winrt::hstring>(json, "id");
        const auto name = JsonUtils::GetValueForKey<winrt::hstring>(json, "name");
        return winrt::make<CustomModel>(id, name.empty() ? id : name);
    }

    CustomModelProvider::CustomModelProvider(winrt::hstring id, winrt::hstring name, winrt::hstring baseUrl) :
        _Id{ std::move(id) },
        _Name{ std::move(name) },
        _BaseUrl{ std::move(baseUrl) }
    {
    }

    Model::CustomModelProvider CustomModelProvider::Copy() const
    {
        auto copy = winrt::make_self<CustomModelProvider>(_Id, _Name, _BaseUrl);
        copy->_ApiContract = _ApiContract;
        copy->_Location = _Location;
        copy->_ApiKeyCredential = _ApiKeyCredential;
        copy->_Models = winrt::single_threaded_vector<Model::CustomModel>();
        for (const auto& model : _Models)
        {
            copy->_Models.Append(winrt::get_self<CustomModel>(model)->Copy());
        }
        return *copy;
    }

    Json::Value CustomModelProvider::ToJson() const
    {
        Json::Value json{ Json::objectValue };
        JsonUtils::SetValueForKey(json, "id", _Id);
        JsonUtils::SetValueForKey(json, "name", _Name);
        JsonUtils::SetValueForKey(json, "baseUrl", _BaseUrl);
        JsonUtils::SetValueForKey(json, "apiContract", _ApiContract);
        JsonUtils::SetValueForKey(json, "location", _Location);
        JsonUtils::SetValueForKey(json, "apiKeyCredential", _ApiKeyCredential);
        JsonUtils::SetValueForKey(json, "models", _Models);
        return json;
    }

    Model::CustomModelProvider CustomModelProvider::FromJson(const Json::Value& json)
    {
        const auto id = JsonUtils::GetValueForKey<winrt::hstring>(json, "id");
        const auto name = JsonUtils::GetValueForKey<winrt::hstring>(json, "name");
        const auto baseUrl = JsonUtils::GetValueForKey<winrt::hstring>(json, "baseUrl");
        const auto displayName = name.empty() ? (baseUrl.empty() ? id : baseUrl) : name;
        auto provider = winrt::make_self<CustomModelProvider>(id, displayName, baseUrl);
        JsonUtils::GetValueForKey(json, "apiContract", provider->_ApiContract);
        JsonUtils::GetValueForKey(json, "location", provider->_Location);
        JsonUtils::GetValueForKey(json, "apiKeyCredential", provider->_ApiKeyCredential);
        JsonUtils::GetValueForKey(json, "models", provider->_Models);
        return *provider;
    }

    winrt::hstring CustomModelProvider::SelectionId(const winrt::hstring& providerId, const winrt::hstring& modelId)
    {
        return ::Microsoft::Terminal::CustomModels::SelectionId(providerId, modelId);
    }

    bool CustomModelProvider::TryParseSelectionId(std::wstring_view selectionId, std::wstring& providerId, std::wstring& modelId)
    {
        return ::Microsoft::Terminal::CustomModels::TryParseSelectionId(selectionId, providerId, modelId);
    }

    winrt::hstring CustomModelProvider::ResolvedLocation(const Model::CustomModelProvider& provider)
    {
        return ::Microsoft::Terminal::CustomModels::ResolvedLocation(provider);
    }
}
