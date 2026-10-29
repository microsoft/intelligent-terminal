// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <iterator>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include <json/json.h>

#include "CustomModelSelection.h"
#include "AgentRegistry.h"

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

    struct StartupFailure
    {
        std::string phase;
        std::string reason;
        uint32_t attempts{};
        uint32_t maxAttempts{};
        bool startupVerified{};
        std::string diagnosticId;
        std::string details;
    };

    inline std::optional<StartupFailure> ParseStartupFailure(const std::string_view payload)
    {
        Json::Value root;
        Json::CharReaderBuilder builder;
        const std::unique_ptr<Json::CharReader> reader{ builder.newCharReader() };
        std::string errors;
        if (!reader->parse(payload.data(), payload.data() + payload.size(), &root, &errors) ||
            !root.isObject() || !root["startup_failure"].isObject())
        {
            return std::nullopt;
        }
        const auto& failure = root["startup_failure"];
        if (!failure["phase"].isString() || !failure["reason"].isString() ||
            !failure["attempts"].isUInt() || !failure["max_attempts"].isUInt() ||
            !failure["startup_verified"].isBool() || !failure["diagnostic_id"].isString())
        {
            return std::nullopt;
        }
        StartupFailure parsed{
            failure["phase"].asString(),
            failure["reason"].asString(),
            failure["attempts"].asUInt(),
            failure["max_attempts"].asUInt(),
            failure["startup_verified"].asBool(),
            failure["diagnostic_id"].asString(),
            {},
        };
        constexpr std::string_view phases[]{ "preparation", "spawn", "initialize", "session", "cleanup" };
        constexpr std::string_view reasons[]{ "failed", "timed_out", "authentication_required", "retry_limit_reached" };
        if (std::ranges::find(phases, parsed.phase) == std::end(phases) ||
            std::ranges::find(reasons, parsed.reason) == std::end(reasons) ||
            parsed.maxAttempts != 3 || parsed.attempts > parsed.maxAttempts ||
            parsed.diagnosticId.size() != 36)
        {
            return std::nullopt;
        }
        for (size_t index = 0; index < parsed.diagnosticId.size(); ++index)
        {
            const auto ch = static_cast<unsigned char>(parsed.diagnosticId[index]);
            const auto separator = index == 8 || index == 13 || index == 18 || index == 23;
            if ((separator && ch != '-') || (!separator && !std::isxdigit(ch)))
            {
                return std::nullopt;
            }
        }
        if (failure.isMember("details") && !failure["details"].isNull())
        {
            if (!failure["details"].isString())
            {
                return std::nullopt;
            }
            parsed.details = failure["details"].asString();
            if (parsed.details.size() > 1024 ||
                std::ranges::any_of(parsed.details, [](const unsigned char ch) {
                    return (ch < 0x20 && ch != '\n') || ch == 0x7f;
                }))
            {
                return std::nullopt;
            }
        }
        return parsed;
    }

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
        const std::optional<std::wstring_view> model = std::nullopt,
        const bool runsInWsl = false)
    {
        if (::Microsoft::Terminal::Settings::Model::AgentRegistry::AgentIdEquals(agentId, L"antigravity"))
        {
            return runsInWsl ? L"agy_acp_server.par --uid=" : L"agy_acp_server.exe";
        }
        if (agentId == L"claude")
        {
            return L"npx -y @agentclientprotocol/claude-agent-acp@0.65.0";
        }
        if (agentId == L"codex")
        {
            return L"npx -y @agentclientprotocol/codex-acp@1.1.13";
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
            commandLine += L" --acp";
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

    inline std::wstring BuildProbeCommandLine(
        const std::wstring_view agentId,
        const std::optional<std::wstring_view> model = std::nullopt,
        const bool runsInWsl = false)
    {
        return BuildAgentCommandLine(
            agentId,
            ::Microsoft::Terminal::Settings::Model::AgentRegistry::SupportsLiveModelSwitch(agentId) ? std::nullopt : model,
            runsInWsl);
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
            (root.isMember("startup_failure") && !root["startup_failure"].isNull()) ||
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
