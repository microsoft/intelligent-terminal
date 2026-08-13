// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <cctype>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include <json/json.h>

#include "CustomModelSelection.h"

namespace Microsoft::Terminal::AcpModels
{
    struct ModelInfo
    {
        std::string id;
        std::string name;
        std::string description;
    };

    struct ModelCatalog
    {
        std::vector<ModelInfo> availableModels;
        std::optional<std::string> currentModelId;
    };

    struct ParseOptions
    {
        bool excludeCustomSelectionIds{ false };
    };

    inline bool StatusUsesHostCatalog(const Json::Value& status)
    {
        if (status.isMember("agent_source"))
        {
            return status["agent_source"].isString() &&
                   status["agent_source"].asString() == "host";
        }
        if (status.isMember("backend"))
        {
            return status["backend"].isString() &&
                   (status["backend"].asString().empty() ||
                    status["backend"].asString() == "Windows");
        }
        // Compatibility with WTA builds that predate source/backend metadata.
        return true;
    }

    inline std::wstring BuildAgentCommandLine(
        const std::wstring_view agentId,
        const std::optional<std::wstring_view> model = std::nullopt)
    {
        if (agentId == L"claude")
        {
            return L"npx -y @agentclientprotocol/claude-agent-acp@0.64.2";
        }
        if (agentId == L"codex")
        {
            return L"npx -y @agentclientprotocol/codex-acp@1.1.9";
        }
        if (agentId == L"opencode")
        {
            return L"opencode acp";
        }

        std::wstring commandLine{ agentId };
        if (agentId == L"copilot")
        {
            commandLine += L" --acp --stdio";
        }
        else if (agentId == L"gemini")
        {
            commandLine += L" --experimental-acp";
        }

        if ((agentId == L"copilot" || agentId == L"gemini") &&
            model.has_value() &&
            !model->empty() &&
            !CustomModels::IsCustomSelection(*model))
        {
            commandLine += L" --model ";
            commandLine += *model;
        }

        return commandLine;
    }

    namespace details
    {
        inline bool IsBlank(const std::string_view value)
        {
            return std::ranges::all_of(value, [](const unsigned char ch) {
                return std::isspace(ch) != 0;
            });
        }
    }

    inline std::optional<ModelCatalog> ParseModelCatalog(
        const Json::Value& root,
        const ParseOptions options = {})
    {
        if (!root.isObject() ||
            !root.isMember("available_models") ||
            !root["available_models"].isArray())
        {
            return std::nullopt;
        }

        ModelCatalog catalog;
        const auto& models = root["available_models"];
        catalog.availableModels.reserve(models.size());
        for (const auto& model : models)
        {
            if (!model.isObject() ||
                !model.isMember("id") ||
                !model["id"].isString())
            {
                continue;
            }

            auto id = model["id"].asString();
            if (details::IsBlank(id) ||
                (options.excludeCustomSelectionIds && CustomModels::IsCustomSelection(id)))
            {
                continue;
            }

            auto name = model.isMember("name") && model["name"].isString() ?
                            model["name"].asString() :
                            std::string{};
            if (details::IsBlank(name))
            {
                name = id;
            }

            auto description = model.isMember("description") && model["description"].isString() ?
                                   model["description"].asString() :
                                   std::string{};
            catalog.availableModels.emplace_back(
                ModelInfo{
                    std::move(id),
                    std::move(name),
                    std::move(description),
                });
        }

        if (root.isMember("current_model_id") && root["current_model_id"].isString())
        {
            catalog.currentModelId = root["current_model_id"].asString();
        }

        return catalog;
    }

    inline std::optional<ModelCatalog> ParseModelCatalog(
        const std::string_view json,
        const ParseOptions options = {})
    {
        if (json.empty())
        {
            return std::nullopt;
        }

        Json::Value root;
        Json::CharReaderBuilder builder;
        const std::unique_ptr<Json::CharReader> reader{ builder.newCharReader() };
        std::string errors;
        if (!reader->parse(json.data(), json.data() + json.size(), &root, &errors))
        {
            return std::nullopt;
        }

        return ParseModelCatalog(root, options);
    }
}
