// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentRegistry.h"
#include "WtaProcess.h"

#include <algorithm>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>

#include <json/json.h>
#include <winrt/base.h>

namespace Microsoft::Terminal::AgentAvailability
{
    struct HostAgentAvailability
    {
        bool nativeCliFound{ false };
        bool launchReady{ false };
        bool requiresNpx{ false };
    };

    struct HostAgentSnapshot
    {
        bool npxFound{ false };
        std::unordered_map<std::wstring, HostAgentAvailability> availability;
    };

    inline const HostAgentAvailability* FindHostAgentAvailability(const HostAgentSnapshot& snapshot,
                                                                 const std::wstring_view agentId) noexcept
    {
        if (const auto it = snapshot.availability.find(std::wstring{ agentId });
            it != snapshot.availability.end())
        {
            return &it->second;
        }
        for (const auto& [id, status] : snapshot.availability)
        {
            if (::Microsoft::Terminal::Settings::Model::AgentRegistry::AgentIdEquals(id, agentId))
            {
                return &status;
            }
        }
        return nullptr;
    }

    inline std::optional<HostAgentSnapshot> ParseHostAgentSnapshot(const std::string_view payload)
    {
        Json::Value root;
        Json::CharReaderBuilder builder;
        const std::unique_ptr<Json::CharReader> reader{ builder.newCharReader() };
        std::string errors;
        if (!reader->parse(payload.data(), payload.data() + payload.size(), &root, &errors) ||
            !root.isObject() ||
            !root["availability"].isArray() ||
            !root["npx_found"].isBool())
        {
            return std::nullopt;
        }

        HostAgentSnapshot snapshot;
        snapshot.npxFound = root["npx_found"].asBool();
        for (const auto& agent : root["availability"])
        {
            if (!agent.isObject())
            {
                return std::nullopt;
            }

            const auto& id = agent["id"];
            const auto& nativeCliFound = agent["native_cli_found"];
            const auto& launchReady = agent["launch_ready"];
            const auto& requiresNpx = agent["requires_npx"];
            if (!id.isString() ||
                !nativeCliFound.isBool() ||
                !launchReady.isBool() ||
                !requiresNpx.isBool())
            {
                return std::nullopt;
            }

            const auto hstringId = winrt::to_hstring(id.asString());
            const std::wstring_view parsedId{ hstringId };
            const auto knownAgent = std::find_if(
                ::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAcpAgents.begin(),
                ::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAcpAgents.end(),
                [&](const auto& candidate) {
                    return ::Microsoft::Terminal::Settings::Model::AgentRegistry::AgentIdEquals(candidate.id, parsedId);
                });
            if (knownAgent == ::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAcpAgents.end())
            {
                continue;
            }

            const HostAgentAvailability status{
                nativeCliFound.asBool(),
                launchReady.asBool(),
                requiresNpx.asBool(),
            };
            if ((status.launchReady && !status.nativeCliFound) ||
                (status.launchReady && status.requiresNpx && !snapshot.npxFound) ||
                !snapshot.availability.emplace(std::wstring{ knownAgent->id }, status).second)
            {
                return std::nullopt;
            }
        }

        // A parseable but partial payload cannot prove that an omitted agent is
        // unavailable. Treat incompatible producer versions as probe-unknown.
        if (snapshot.availability.size() !=
            ::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAcpAgents.size())
        {
            return std::nullopt;
        }
        return snapshot;
    }

    inline std::vector<::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAgent>
    BuildFreAgentCandidates(
        const std::vector<::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAgent>& allowedAgents,
        const std::optional<HostAgentSnapshot>& snapshot,
        const std::wstring_view currentSelection,
        const std::wstring_view effectiveAgent)
    {
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
        std::vector<Reg::BuiltinAgent> result;

        const auto appendAllowed = [&](const std::wstring_view id) {
            if (id.empty())
            {
                return;
            }
            const auto allowed = std::find_if(
                allowedAgents.begin(),
                allowedAgents.end(),
                [&](const auto& candidate) {
                    return Reg::AgentIdEquals(candidate.id, id);
                });
            if (allowed == allowedAgents.end() ||
                std::any_of(
                    result.begin(),
                    result.end(),
                    [&](const auto& existing) {
                        return Reg::AgentIdEquals(existing.id, allowed->id);
                    }))
            {
                return;
            }
            result.push_back(*allowed);
        };

        if (!snapshot)
        {
            appendAllowed(currentSelection);
            appendAllowed(effectiveAgent);
            appendAllowed(L"copilot");
            if (result.empty() && !allowedAgents.empty())
            {
                result.push_back(allowedAgents.front());
            }
            return result;
        }

        for (const auto& agent : allowedAgents)
        {
            const auto status = FindHostAgentAvailability(*snapshot, agent.id);
            if (Reg::AgentIdEquals(agent.id, L"copilot") ||
                (status && status->nativeCliFound))
            {
                result.push_back(agent);
            }
        }
        return result;
    }

    struct FreAgentSelectionDecision
    {
        bool persistSelection{ false };
        std::wstring setupAgentId;
    };

    inline FreAgentSelectionDecision DecideFreAgentSelection(
        const std::vector<::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAgent>& allowedAgents,
        const std::wstring_view displayedAgent,
        const std::wstring_view effectiveAgent,
        const bool explicitlyChanged)
    {
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
        const auto allowed = std::find_if(
            allowedAgents.begin(),
            allowedAgents.end(),
            [&](const auto& candidate) {
                return Reg::AgentIdEquals(candidate.id, displayedAgent);
            });
        if (allowed == allowedAgents.end())
        {
            return {};
        }

        FreAgentSelectionDecision decision;
        decision.persistSelection = explicitlyChanged;
        if (explicitlyChanged || Reg::AgentIdEquals(allowed->id, effectiveAgent))
        {
            decision.setupAgentId = allowed->id;
        }
        return decision;
    }

    inline bool NeedsFreNodeBootstrap(const std::optional<HostAgentSnapshot>& snapshot,
                                      const std::wstring_view agentId) noexcept
    {
        if (!snapshot ||
            (!::Microsoft::Terminal::Settings::Model::AgentRegistry::AgentIdEquals(agentId, L"claude") &&
             !::Microsoft::Terminal::Settings::Model::AgentRegistry::AgentIdEquals(agentId, L"codex")))
        {
            return false;
        }

        const auto status = FindHostAgentAvailability(*snapshot, agentId);
        return status &&
               status->nativeCliFound &&
               status->requiresNpx &&
               !snapshot->npxFound;
    }

    inline std::unordered_set<std::wstring> ParseHostAgentIds(const std::string_view payload)
    {
        Json::Value root;
        Json::CharReaderBuilder builder;
        const std::unique_ptr<Json::CharReader> reader{ builder.newCharReader() };
        std::string errors;
        if (!reader->parse(payload.data(), payload.data() + payload.size(), &root, &errors) ||
            !root.isObject() ||
            !root["agents"].isArray())
        {
            return {};
        }

        std::unordered_set<std::wstring> ids;
        for (const auto& agent : root["agents"])
        {
            const auto& id = agent["id"];
            if (id.isString())
            {
                const auto hstringId = winrt::to_hstring(id.asString());
                ids.emplace(hstringId.c_str(), hstringId.size());
            }
        }
        return ids;
    }

    inline std::optional<HostAgentSnapshot> ProbeHostAgentSnapshot()
    {
        const auto wtaPath = WtaProcess::ResolveWtaExePath();
        if (wtaPath.empty())
        {
            return std::nullopt;
        }

        const auto output = WtaProcess::RunWtaCaptureStdout(
            wtaPath,
            L"probe-host-agents",
            2'000);
        return ParseHostAgentSnapshot(output);
    }

    inline std::unordered_set<std::wstring> ProbeHostAgentIds()
    {
        const auto wtaPath = WtaProcess::ResolveWtaExePath();
        if (wtaPath.empty())
        {
            return {};
        }

        const auto output = WtaProcess::RunWtaCaptureStdout(
            wtaPath,
            L"probe-host-agents",
            2'000);
        return ParseHostAgentIds(output);
    }
}
