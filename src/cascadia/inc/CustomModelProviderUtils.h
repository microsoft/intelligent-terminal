// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <cwctype>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include <gsl/narrow>
#include <json/json.h>
#include <winrt/Microsoft.Terminal.Settings.Model.h>
#include <winrt/base.h>

#include "CustomModelProviderContract.h"
#include "CustomModelSelection.h"

namespace Microsoft::Terminal::CustomModels
{
    struct LaunchConfiguration
    {
        std::wstring selectionId;
        std::wstring endpoint;
        std::wstring modelId;
        std::wstring credentialId;
        bool apiKeyRequired{ false };

        bool operator==(const LaunchConfiguration&) const = default;
    };

    struct CatalogEntry
    {
        std::string selectionId;
        std::string providerId;
        std::string providerName;
        std::string apiContract;
        std::string location;
        std::string modelId;
        std::string modelName;

        bool operator==(const CatalogEntry&) const = default;
    };

    inline winrt::hstring ProviderDisplayNameFromEndpoint(
        const std::wstring_view endpoint,
        const std::wstring_view fallback)
    {
        const auto isWhitespace = [](const wchar_t ch) {
            return std::iswspace(ch);
        };
        const auto first = std::ranges::find_if_not(endpoint, isWhitespace);
        const auto last = std::ranges::find_if_not(endpoint.rbegin(), endpoint.rend(), isWhitespace).base();
        if (first >= last)
        {
            return winrt::hstring{ fallback };
        }

        auto value = endpoint.substr(
            gsl::narrow_cast<size_t>(first - endpoint.begin()),
            gsl::narrow_cast<size_t>(last - first));
        const auto scheme = value.find(L"://");
        if (scheme != std::wstring_view::npos)
        {
            value.remove_prefix(scheme + 3);
        }

        if (const auto authorityEnd = value.find_first_of(L"/?#");
            authorityEnd != std::wstring_view::npos)
        {
            value = value.substr(0, authorityEnd);
        }
        if (const auto userInfo = value.rfind(L'@');
            userInfo != std::wstring_view::npos)
        {
            value.remove_prefix(userInfo + 1);
        }

        std::wstring_view host;
        if (value.starts_with(L'['))
        {
            if (const auto closingBracket = value.find(L']');
                closingBracket != std::wstring_view::npos)
            {
                host = value.substr(1, closingBracket - 1);
            }
        }
        else
        {
            const auto firstColon = value.find(L':');
            const auto lastColon = value.rfind(L':');
            host = firstColon != std::wstring_view::npos && firstColon == lastColon ?
                       value.substr(0, firstColon) :
                       value;
        }

        return host.empty() ? winrt::hstring{ fallback } : winrt::hstring{ host };
    }

    inline winrt::hstring SelectionId(const winrt::hstring& providerId, const winrt::hstring& modelId)
    {
        std::wstring value{ L"custom:" };
        value.append(providerId);
        value.push_back(L':');
        value.append(modelId);
        return winrt::hstring{ value };
    }

    inline LaunchConfiguration MakeLaunchConfiguration(
        const std::wstring_view selectionId,
        const std::wstring_view endpoint,
        const std::wstring_view modelId,
        const std::wstring_view credentialId,
        const bool apiKeyRequired)
    {
        return LaunchConfiguration{
            std::wstring{ selectionId },
            std::wstring{ endpoint },
            std::wstring{ modelId },
            std::wstring{ credentialId },
            apiKeyRequired,
        };
    }

    inline LaunchConfiguration MakeLaunchConfiguration(
        const std::wstring_view selectionId,
        const winrt::Microsoft::Terminal::Settings::Model::CustomModelProvider& provider,
        const winrt::Microsoft::Terminal::Settings::Model::CustomModel& model)
    {
        return MakeLaunchConfiguration(
            selectionId,
            std::wstring_view{ provider.BaseUrl() },
            std::wstring_view{ model.Id() },
            std::wstring_view{ provider.ApiKeyCredential() },
            provider.ApiKeyRequired());
    }

    template<typename TRange>
    std::vector<CatalogEntry> CaptureCatalog(const TRange& providers)
    {
        std::vector<CatalogEntry> catalog;
        for (const auto& provider : providers)
        {
            if (!IsSupportedApiContract(std::wstring_view{ provider.ApiContract() }))
            {
                continue;
            }

            for (const auto& model : provider.Models())
            {
                catalog.emplace_back(CatalogEntry{
                    winrt::to_string(SelectionId(provider.Id(), model.Id())),
                    winrt::to_string(provider.Id()),
                    winrt::to_string(provider.Name()),
                    winrt::to_string(provider.ApiContract()),
                    winrt::to_string(provider.Location()),
                    winrt::to_string(model.Id()),
                    winrt::to_string(model.Name()),
                });
            }
        }
        return catalog;
    }

    template<typename TRange>
    bool SelectionExists(
        const TRange& providers,
        const std::wstring_view selectionId,
        const bool requireSupportedContract = false)
    {
        std::wstring providerId;
        std::wstring modelId;
        if (!TryParseSelectionId(selectionId, providerId, modelId))
        {
            return false;
        }

        for (const auto& provider : providers)
        {
            if (provider.Id() != providerId ||
                (requireSupportedContract &&
                 !IsSupportedApiContract(std::wstring_view{ provider.ApiContract() })))
            {
                continue;
            }

            return std::ranges::any_of(provider.Models(), [&](const auto& model) {
                return model.Id() == modelId;
            });
        }
        return false;
    }

    inline std::vector<std::pair<std::wstring, std::wstring>> BuildHelperModelBootstrapArguments(
        const std::wstring_view selectionId,
        const std::span<const CatalogEntry> catalog,
        const bool useHostCatalog)
    {
        if (!useHostCatalog || selectionId.empty())
        {
            return {};
        }

        const auto selectionUtf8 = winrt::to_string(winrt::hstring{ selectionId });
        if (!std::ranges::any_of(catalog, [&](const auto& model) {
                return model.selectionId == selectionUtf8;
            }))
        {
            return {};
        }

        return {
            { L"--custom-model-selection", std::wstring{ selectionId } },
        };
    }

    template<typename TOriginalRange, typename TVisibleRange>
    std::vector<winrt::Microsoft::Terminal::Settings::Model::CustomModelProvider>
    MergeProviderEditsPreservingUnsupported(
        const TOriginalRange& originalProviders,
        const TVisibleRange& visibleProviders)
    {
        using Provider = winrt::Microsoft::Terminal::Settings::Model::CustomModelProvider;

        std::vector<Provider> remainingVisible{ visibleProviders.begin(), visibleProviders.end() };
        std::vector<Provider> merged;
        for (const auto& original : originalProviders)
        {
            if (!IsSupportedApiContract(std::wstring_view{ original.ApiContract() }))
            {
                merged.emplace_back(original);
                continue;
            }

            const auto replacement = std::find_if(
                remainingVisible.begin(),
                remainingVisible.end(),
                [&](const auto& provider) {
                    return provider.Id() == original.Id();
                });
            if (replacement != remainingVisible.end())
            {
                merged.emplace_back(*replacement);
                remainingVisible.erase(replacement);
            }
        }

        merged.insert(merged.end(), remainingVisible.begin(), remainingVisible.end());
        return merged;
    }

    inline winrt::hstring FormatModelDisplayText(
        const winrt::Microsoft::Terminal::Settings::Model::CustomModelProvider& provider)
    {
        std::wstring display;
        for (const auto& model : provider.Models())
        {
            if (!display.empty())
            {
                display.push_back(L'\n');
            }

            const auto name = model.Name();
            const auto id = model.Id();
            if (!name.empty() && name != id)
            {
                display.append(name);
                display.append(L" (");
                display.append(id);
                display.push_back(L')');
            }
            else
            {
                display.append(id);
            }
        }
        return winrt::hstring{ display };
    }

    inline Json::Value CatalogToJson(const std::span<const CatalogEntry> catalog)
    {
        Json::Value options{ Json::arrayValue };
        for (const auto& entry : catalog)
        {
            Json::Value option{ Json::objectValue };
            option["selection_id"] = entry.selectionId;
            option["provider_id"] = entry.providerId;
            option["provider_name"] = entry.providerName;
            option["api_contract"] = entry.apiContract;
            option["location"] = entry.location;
            option["model_id"] = entry.modelId;
            option["name"] = entry.modelName;
            options.append(std::move(option));
        }
        return options;
    }

    inline std::string SerializeCatalog(const std::span<const CatalogEntry> catalog)
    {
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        return Json::writeString(writer, CatalogToJson(catalog));
    }

    inline winrt::hstring ResolvedLocation(
        const winrt::Microsoft::Terminal::Settings::Model::CustomModelProvider& provider)
    {
        const auto configured = provider.Location();
        if (configured == L"local" || configured == L"cloud")
        {
            return configured;
        }

        auto url = std::wstring{ provider.BaseUrl() };
        std::transform(url.begin(), url.end(), url.begin(), [](const wchar_t ch) {
            return gsl::narrow_cast<wchar_t>(std::towlower(ch));
        });
        const bool local =
            url.find(L"://localhost") != std::wstring::npos ||
            url.find(L"://127.") != std::wstring::npos ||
            url.find(L"://[::1]") != std::wstring::npos ||
            url.find(L"://0.0.0.0") != std::wstring::npos ||
            url.find(L".local") != std::wstring::npos;
        return local ? L"local" : L"cloud";
    }
}
